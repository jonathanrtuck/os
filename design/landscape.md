# Landscape Comparison

A technical comparison between this OS and existing systems. Written for someone
discovering the project who wants to understand where it fits — what kind of OS
this is, what it shares with systems they know, and where it deliberately
diverges.

This is not a feature comparison or a marketing document. It's an honest
accounting of design choices, their consequences, and how they stack up against
the alternatives.

---

## Quick Classification

The questions a systems programmer would ask on first encounter:

| Question                            | Answer                                                                                                                      |
| ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| What kind of kernel?                | Microkernel. Rust, `no_std`, arm64. 34 syscalls, ~29K LOC.                                                                  |
| POSIX?                              | No. Custom syscall ABI designed around handles and sync IPC.                                                                |
| More like Linux, macOS, or Fuchsia? | Microkernel + capabilities like Fuchsia. MIME-aware content model like BeOS. Document-centric data model like none of them. |
| Filesystem?                         | Custom COW with per-document snapshots, metadata queries, and a document store layer. Implemented.                          |
| GPU?                                | Metal via hypervisor passthrough, scene graph rendering. Implemented.                                                       |
| Can it run existing software?       | No. No POSIX, no libc, no compatibility layer. Everything is `no_std` Rust.                                                 |
| Performance?                        | Kernel hot paths cycle-benchmarked on M4 Pro. IPC round-trip, page fault, object lifecycle all baselined.                   |
| How much code?                      | ~90K LOC Rust (~29K kernel + ~61K userspace). 558 kernel tests, 4 fuzz targets, 33 property tests.                          |
| Is this serious?                    | Design exploration with a verified kernel. Not a product, not a weekend project. Closer to a research OS.                   |

---

## Systems Compared

| System               | Era   | Relevance                                                                                        |
| -------------------- | ----- | ------------------------------------------------------------------------------------------------ |
| **Linux**            | 1991– | The baseline. Monolithic, POSIX, app-centric. What most people compare against.                  |
| **macOS (XNU)**      | 2001– | Development host. APFS has COW. Metal GPU. Closest mainstream for rendering.                     |
| **Windows (NT)**     | 1993– | Hybrid kernel. App-centric with shell extensions.                                                |
| **Fuchsia (Zircon)** | 2016– | Modern microkernel with capabilities. Closest in kernel architecture. Ships on Nest Hub.         |
| **Redox**            | 2015– | Rust microkernel. Closest in language choice. POSIX-compatible (different thesis).               |
| **seL4**             | 2009– | Formally verified microkernel. Gold standard for kernel correctness proofs.                      |
| **BeOS / Haiku**     | 1996– | MIME-based content awareness, typed file attributes, query navigation. Closest in content model. |
| **Plan 9**           | 1992– | Everything-is-a-file, per-process namespaces, 9P. Philosophical ancestor.                        |
| **Oberon**           | 1988– | Radical minimalism, text-as-command, no CLI/GUI distinction.                                     |

Historical and conceptual systems (Xerox Star, OpenDoc, Mercury OS, Ideal OS)
informed the design but are not included here because they lack implementations
to compare against technically.

---

## Kernel & Process Model

**Our approach:** Preemptive microkernel in Rust (`no_std`,
`aarch64-unknown-none`). SMP (up to 8 cores), per-core fixed-priority preemptive
scheduler with 4 levels (Idle/Low/Medium/High). 34 syscalls across 6 object
types (VMO, Endpoint, Event, Thread, Address Space, Resource). Kernel spawns
only init; init spawns everything else. Hardware isolation via ARM EL0/EL1. Full
context save/restore including NEON/FP state. Framekernel discipline: all
`unsafe` code confined to `frame/` module, enforced by `#![deny(unsafe_code)]`
at crate root.

|                     | This OS                 | Linux                | macOS (XNU)         | Fuchsia (Zircon)  | Redox                    | seL4                         |
| ------------------- | ----------------------- | -------------------- | ------------------- | ----------------- | ------------------------ | ---------------------------- |
| Architecture        | Microkernel             | Monolithic           | Hybrid (Mach + BSD) | Microkernel       | Microkernel              | Microkernel                  |
| Language            | Rust (`no_std`)         | C                    | C/C++               | C++               | Rust                     | C (verified)                 |
| Syscalls            | 34                      | ~450                 | ~540 (Mach + BSD)   | ~170              | POSIX-like set           | ~12                          |
| Scheduler           | Per-core fixed-priority | EEVDF (since 6.6)    | Mach decay-usage    | Fair scheduler    | Cooperative + preemptive | Round-robin (minimal)        |
| SMP                 | Up to 8 cores           | Thousands            | Dozens              | Many              | Single core (WIP)        | Configurable                 |
| Boot init model     | Init from service pack  | PID 1 (systemd/init) | launchd             | component_manager | initfs                   | Root task                    |
| Formal verification | No                      | No                   | No                  | No                | No                       | Yes (functional correctness) |

**Convergences:** The microkernel + root-task-spawns-everything pattern is
shared with Fuchsia and seL4. The kernel uses a fixed-priority scheduler; EEVDF
(as used by Linux 6.6+) is a future option if workload data justifies the
complexity.

**Tradeoffs:** 34 syscalls vs Linux's ~450 reflects scope, not minimalism for
its own sake — networking, multi-user, and pipes don't exist yet. The count will
grow. Rust prevents memory safety bugs in the kernel (shared with Redox), but
the kernel is not formally verified (seL4's advantage). The design bets that
Rust's type system + framekernel discipline + 12-phase verification (558 tests,
4 fuzz targets, 33 property tests, Miri, mutation testing, sanitizers, SMP
stress) provides adequate confidence for a single-user system — a weaker
guarantee than seL4's formal proofs, but at a fraction of the engineering cost.

---

## Memory & Security

**Our approach:** Capability-based handle table with rights attenuation —
processes hold handles to VMOs (virtual memory objects), endpoints, events,
threads, and address spaces. 9 named rights (READ, WRITE, EXECUTE, MAP, DUP,
TRANSFER, SIGNAL, WAIT, SPAWN) monotonically attenuated on duplication or
transfer. No ambient authority (no global PID namespace, no `/proc`, no ambient
file access). Split TTBR (kernel TTBR1 / user TTBR0). VMO-backed demand-paged
memory with COW snapshots and pager interface. W^X enforcement on all pages.
ASLR (per-process user-space + kernel KASLR). PAC (pointer authentication) and
BTI (branch target identification) on ARM64. No dynamic linking, no shared
libraries.

|                 | This OS                   | Linux                        | macOS                       | Fuchsia                 | seL4                             |
| --------------- | ------------------------- | ---------------------------- | --------------------------- | ----------------------- | -------------------------------- |
| Isolation       | EL0/EL1 hardware          | Ring 0/3 hardware            | Hardware + SIP + sandbox    | Hardware + capabilities | Hardware + verified capabilities |
| Capabilities    | Handle + 9 rights         | DAC + MAC (SELinux/AppArmor) | Sandbox entitlements        | Handle table (runtime)  | Capabilities (verified)          |
| Memory objects  | VMOs (COW, sealed, pager) | Anonymous/file mmap          | Mach VM objects             | VMOs                    | Frames + untyped                 |
| Page size       | 16 KiB                    | 4 KiB default                | 16 KiB (Apple Silicon)      | 4 KiB                   | Configurable                     |
| W^X             | Enforced on all pages     | Optional (mprotect)          | Enforced (hardened runtime) | Enforced                | Enforced                         |
| ASLR            | Planned                   | Yes                          | Yes                         | Yes                     | N/A                              |
| PAC / BTI       | Planned                   | Yes (since 5.8/5.10)         | Yes (Apple Silicon)         | No                      | N/A                              |
| Dynamic linking | None                      | ld.so                        | dyld                        | ELF + vDSO              | None                             |

**Tradeoffs:** The handle-based model is structurally similar to Fuchsia's: you
cannot access a resource you don't hold a handle to. Rights attenuation and
per-handle badges go beyond Fuchsia's base model — badges enable userspace
servers to identify callers without a global PID namespace. VMOs with versioning
(COW snapshot ring), sealing (immutable freeze), content typing (u64 tag), and
userspace pagers are architecturally comparable to Fuchsia's VMOs but add
several novel features (bounded snapshot ring, compile-time seal enforcement via
rights). ASLR and PAC/BTI are planned for userspace bring-up. No dynamic linking
eliminates GOT/PLT hijacking and LD_PRELOAD-class attacks, at the cost of higher
memory usage when multiple processes share library code (each gets its own copy
in physical memory).

---

## Inter-Process Communication

**Our approach:** The kernel provides synchronous IPC via endpoints
(call/recv/reply with priority inheritance) and asynchronous notification via
events (64-bit signal bitmask with multi-wait). The userspace design layers two
mechanisms on top: event rings (64-byte lock-free SPSC ring buffers in shared
memory) for discrete events where order matters, and state registers (atomic
shared memory with store-release / load-acquire) for continuous state where only
the latest value matters.

|                    | This OS                            | Linux                         | macOS (XNU)                  | Fuchsia               | Plan 9            |
| ------------------ | ---------------------------------- | ----------------------------- | ---------------------------- | --------------------- | ----------------- |
| Primary mechanism  | Sync IPC (call/recv/reply)         | Pipes, sockets, shared memory | Mach ports (queued messages) | Channels (FIDL-typed) | 9P file protocol  |
| Message size       | Fixed 64 bytes                     | Arbitrary                     | Arbitrary (Mach messages)    | Arbitrary (FIDL)      | Arbitrary (9P)    |
| Bulk data          | Shared memory (zero-copy)          | sendfile, splice, mmap        | Mach OOL memory              | VMOs (zero-copy)      | Read/write on fd  |
| Typed messages     | Yes (protocol library, 10 modules) | No (byte streams)             | Partial (MIG)                | Yes (FIDL, versioned) | No (byte streams) |
| Async notification | Event signals + endpoint binding   | epoll / io_uring              | Mach port sets               | port_wait             | sleep/wakeup      |

**Convergences:** Zero-copy bulk data via shared memory is the same design as
Fuchsia's VMOs and Spring OS's VM/IPC unification. Typed message definitions
parallel Fuchsia's FIDL and Singularity's channel contracts.

**Tradeoffs:** Fixed 64-byte messages keep the ring buffer simple and
cache-line-friendly but cap inline payload at ~60 bytes — anything larger goes
through shared memory with an extra indirection. Fuchsia's FIDL allows arbitrary
message sizes with automatic marshaling and versioning. The two-mechanism split
(event rings for discrete, state registers for continuous) is unusual — most
systems use one IPC mechanism for everything. The argument: pointer position
updates at 60+ Hz should not queue behind a backlog of key events, and key
events should not be silently dropped because a newer one overwrote them.

---

## Storage & Filesystem

**Our approach:** Custom COW filesystem. Seven-layer stack: block device trait →
superblock ring (crash consistency) → free-extent allocator → inodes → COW write
path → snapshots → Files trait. On top: a document store that adds media type
tracking, queryable metadata (equality, comparison, AND/OR), and file identity
(FileId). Disk images built by a factory tool (`mkdisk`) that pre-populates
fonts and content.

|                  | This OS                | Linux (ext4)                  | Linux (btrfs)          | macOS (APFS)               | Redox (RedoxFS) | Haiku (BFS)                       |
| ---------------- | ---------------------- | ----------------------------- | ---------------------- | -------------------------- | --------------- | --------------------------------- |
| COW              | Block-level            | No (journaled)                | Extent-based           | Extent-based               | Yes             | No (journaled)                    |
| Snapshots        | Per-document           | No                            | Per-subvolume          | Per-volume                 | No              | No                                |
| Metadata queries | Built-in (store layer) | External (locate, mlocate)    | No                     | Spotlight (separate index) | No              | Built-in (B+ tree indexes)        |
| Typed attributes | Via document store     | xattrs (untyped blobs)        | xattrs (untyped blobs) | xattrs (untyped blobs)     | No              | Yes (typed, indexed)              |
| Live queries     | Not yet                | inotify (events, not queries) | No                     | FSEvents (directory-level) | No              | Yes (kernel-level B_QUERY_UPDATE) |
| Crash recovery   | Superblock ring        | Journal replay                | COW tree + checksums   | COW + checksums            | Header backup   | Journal replay                    |
| Encryption       | No                     | LUKS (block layer)            | No                     | Per-file                   | No              | No                                |
| Compression      | No                     | No                            | zstd, lzo, zlib        | lzfse, zlib                | No              | No                                |

**Convergences:** COW + snapshots is the same fundamental mechanism as btrfs and
APFS. The metadata query model independently arrived at the same design as
Haiku's BFS — typed attributes, indexed, queryable from the filesystem layer.
This convergence from different starting points suggests both designs found the
same underlying shape.

**Tradeoffs:** Per-document snapshots are the mechanism that enables OS-level
undo (see below). No mainstream filesystem snapshots at this granularity — btrfs
and APFS snapshot entire subvolumes or volumes. The cost: every edit preserves
old data until snapshots are pruned, and the pruning policy is not yet designed.
The filesystem is young and unproven at scale. Mature COW filesystems (btrfs,
ZFS, APFS) encode decades of edge-case handling — data integrity under power
loss, fragmentation management, space accounting under snapshot pressure — that
this implementation has not yet confronted. No encryption or compression; both
are leaf-node features that can be added behind the block interface without
architectural change.

---

## Content Model & File Understanding

**Our approach:** The OS natively understands content types via IANA mimetypes.
Every document has a media type as OS-managed metadata (not a file extension
convention, not an app association). The OS can render any file it understands
without installing an application. Documents are manifests that reference
content files and describe their relationships along three axes: spatial,
temporal, and logical.

This is the core thesis: **OS → Document → Tool** instead of OS → App →
Document. The OS manages content directly; applications are tools you attach to
content, not containers you put content inside.

|                     | This OS                  | Linux                  | macOS                   | Windows                    | BeOS/Haiku                     |
| ------------------- | ------------------------ | ---------------------- | ----------------------- | -------------------------- | ------------------------------ |
| Content identity    | Mimetype (OS-managed)    | Extension (convention) | UTI + extension         | Extension + registry       | Mimetype (FS attribute)        |
| OS renders content? | Yes (all known types)    | No (needs app)         | Partial (Quick Look)    | Partial (preview pane)     | Partial (Translation Kit)      |
| File → app binding  | Mimetype → editor plugin | Extension → .desktop   | UTI → app bundle        | Extension → registry → app | Mimetype → preferred app       |
| Compound documents  | Manifest + content refs  | App-level only         | App-level (NSDocument)  | App-level (OLE)            | App-level                      |
| Format conversion   | Translators (planned)    | App-level              | Quick Actions (limited) | App-level                  | Translation Kit (system-level) |

**Where this diverges:** In Linux, macOS, and Windows, the OS treats files as
opaque byte streams. Applications give them meaning. This OS inverts that: the
OS is the reader, the OS is the renderer, and applications exist only to edit.
The closest precedent is BeOS, which combined MIME-based typing and system-level
format translation — though BeOS still used an app-centric interaction model at
the UI layer.

**Tradeoffs:** Native content understanding means the OS must ship decoders and
renderers for every supported format — currently plain text, rich text, and PNG.
Each is a sandboxed leaf node behind a standard interface, but the total
investment grows linearly with format count. Mainstream OSes externalize this
cost to application developers. The benefit: content is never trapped inside an
application, and viewing any supported format requires zero installation.

---

## Display & Rendering

**Our approach:** The OS compiles document state into a scene graph — a tree of
positioned, decorated, content-agnostic visual nodes in shared memory. A render
service walks this tree and produces pixels via the Metal GPU. Applications
never touch pixels; the OS is the sole renderer. The analogy: the OS service is
a compiler (document → scene graph), the render service is a CPU (scene graph →
pixels).

Scene graph nodes carry geometry (position, size in millipoints — 1/1024 of a
typographic point), visual decoration (background, border, corner radius,
opacity, blur), content (rasterized glyphs, pixel buffers, vector paths), and
accessibility metadata (role, level, state, name).

|                   | This OS                                                  | Linux (Wayland)                | macOS (Quartz)           | Fuchsia (Flatland)                  | BeOS/Haiku         |
| ----------------- | -------------------------------------------------------- | ------------------------------ | ------------------------ | ----------------------------------- | ------------------ |
| Who renders?      | OS only                                                  | Apps (client-side)             | Apps (AppKit/Metal)      | Apps (Scenic)                       | Apps (app_server)  |
| Composition       | Scene graph in shared memory                             | Buffer exchange (wl_surface)   | WindowServer compositing | Flatland scene graph                | Server-side layers |
| GPU API           | Metal (hypervisor passthrough)                           | Vulkan/OpenGL (Mesa)           | Metal                    | Vulkan                              | OpenGL (limited)   |
| Text rendering    | OS (custom rasterizer + layout)                          | App-chosen (FreeType/HarfBuzz) | App via Core Text        | App-chosen                          | App via FreeType   |
| Resolution model  | Points (1/72 in) everywhere; pixels only at render edge  | DPI-aware (app responsibility) | Points (72 DPI base)     | Logical pixels (app responsibility) | Fixed DPI          |
| Coordinate system | Millipoints (1/1024 pt), exactly two units in the system | Varies by toolkit              | Points (CGFloat)         | Logical pixels                      | Pixels             |
| Visual effects    | OS-level (blur, corner radius, opacity, clip masks)      | Compositor (Picom, Mutter)     | WindowServer             | Flatland                            | Minimal            |

**Where this diverges:** Most OSes are buffer compositors — applications render
into off-screen buffers, and a compositor stitches them together. This OS is a
scene compositor — applications describe _what_ to show, and the OS decides
_how_ to render it. Fuchsia's Flatland is the closest mainstream comparison
(apps submit scene graph contributions), but Flatland still allows apps to
render into buffers.

**Tradeoffs:** OS-controlled rendering guarantees visual consistency (identical
typography, color handling, and effects everywhere), enables accessibility by
construction (the renderer knows the structure), and makes the entire display
pipeline a single optimization target. The cost: applications cannot implement
custom rendering. No game engines, no CAD viewports, no custom canvases. Every
visual capability must be a scene graph primitive. The system targets document
workflows. Content types that need novel visual representation (3D models, node
graphs, waveform displays) would require extending the scene graph — an
architectural cost that mainstream systems don't pay because apps render
themselves.

---

## Application / Editor Model

**Our approach:** Editors are untrusted, isolated, restartable leaf nodes. An
editor has read-only memory-mapped access to the document buffer and sends write
requests to the OS service via IPC. The OS service is the sole writer to
document state. Editors understand one content type (text, images, audio). They
are structurally parallel to device drivers: a driver translates hardware
signals into OS primitives; an editor translates user gestures into write
requests. Both are sandboxed, crash-safe, and replaceable.

|                 | This OS                             | Linux/macOS/Windows           | Fuchsia                     | Plan 9                 |
| --------------- | ----------------------------------- | ----------------------------- | --------------------------- | ---------------------- |
| App = ?         | Editor (content-type tool)          | Full application              | Component                   | File server            |
| Trust model     | Untrusted (read-only doc access)    | Trusted (full file write)     | Sandboxed (capabilities)    | Sandboxed (namespace)  |
| Crash impact    | Editor restarts; document is intact | Data loss likely              | Component restarts          | Service restarts       |
| Write path      | Editor → IPC → OS → disk (COW)      | App → syscall → kernel → disk | App → FIDL → service → disk | App → 9P → file server |
| State ownership | OS owns document + view state       | App owns everything           | App owns most state         | File servers own data  |

**Tradeoffs:** This model gives the OS full authority over documents: undo,
crash recovery, access control, and format conversion are OS services, not app
features. The cost: editors are constrained. They cannot maintain arbitrary
internal state across sessions, cannot render custom UI, and must conform to the
edit protocol (beginOp/endOp boundaries). Applications like Photoshop — with
layer state, custom rendering, tool palettes, and project files — are a poor fit
in this model. The design argues that this complexity should live in the content
type (a rich image format with layers) and the OS's layout engine, not in the
application. Whether this holds for every use case is an open question.

---

## Undo & History

**Our approach:** Undo is an OS primitive. The OS takes COW filesystem snapshots
at edit operation boundaries. Cmd+Z restores the previous snapshot. 64-entry
undo ring with character-level granularity. Undo works across editor switches —
because the OS owns document state and undo history, changing editors does not
lose or reset the undo stack.

|                      | This OS                                     | Mainstream (Linux/macOS/Windows)    | APFS / btrfs                                |
| -------------------- | ------------------------------------------- | ----------------------------------- | ------------------------------------------- |
| Who implements undo? | OS (filesystem-level COW)                   | Each app independently              | N/A (snapshots exist but not wired to undo) |
| Granularity          | Operation boundaries (coalesced keystrokes) | Varies wildly per app               | Volume/subvolume snapshots                  |
| Cross-editor undo?   | Yes                                         | No                                  | N/A                                         |
| Crash recovery       | Full (COW state survives)                   | Varies (autosave in some apps)      | Snapshots survive crashes                   |
| Selective undo       | Not yet (requires content-type rebase)      | Some apps (e.g., Photoshop history) | No                                          |

**Where this is novel:** No mainstream OS provides undo at the OS level. APFS
and btrfs have COW and snapshots, but they operate at volume or subvolume
granularity — not per-document, not wired to keyboard shortcuts. This is the
clearest payoff of the document-centric model: because the OS owns all document
writes and mediates them through operation boundaries, undo falls out of the
filesystem's existing COW mechanism.

**Tradeoffs:** COW snapshots are simple and robust for text, where each undo
point is small. For large binary content (a multi-megabyte image), each
operation boundary snapshots the entire file's modified blocks. Snapshot pruning
policy (when to discard old undo points) is not yet designed. Selective undo
(reversing one specific edit while keeping later ones) requires
content-type-aware rebase handlers — the same machinery needed for collaborative
editing. Designed for, not yet built.

---

## Input & Accessibility

**Our approach:** All input flows through the OS. The presenter routes events
to: system gestures, cursor navigation, or the active editor. Navigation (arrow
keys, word/line/document movement, selection expansion) is OS-owned — editors
receive only edit intents (insert, delete, replace), not raw key events.
Accessibility metadata (role, heading level, state flags, accessible name, node
relations) is native to scene graph nodes, not a separate tree.

|                    | This OS                         | Linux                     | macOS                           | Windows                   |
| ------------------ | ------------------------------- | ------------------------- | ------------------------------- | ------------------------- |
| Input routing      | OS-level (presenter)            | Window manager + toolkit  | NSResponder chain               | HWND message pump         |
| Text navigation    | OS-owned                        | App/toolkit-owned         | App/toolkit-owned               | App/toolkit-owned         |
| A11y data model    | Scene graph native (every node) | Separate tree (AT-SPI2)   | Separate tree (NSAccessibility) | Separate tree (UIA)       |
| Screen reader path | Same data as visual rendering   | Parallel tree (can drift) | Parallel tree (can drift)       | Parallel tree (can drift) |

**Tradeoffs:** OS-owned navigation means identical keyboard behavior in every
editor and every content type — no application gets navigation wrong or
different. The cost: editors cannot customize navigation for specialized content
(a music timeline, a node graph editor). For standard content types (text,
lists, tables) the OS provides correct behavior. For domain-specific navigation,
this is a known pressure point.

Accessibility in mainstream OSes is a parallel data model that applications
populate — and frequently get wrong or skip entirely. Because this OS renders
everything through the scene graph, and the scene graph carries semantic data,
the accessibility representation is the visual representation. They cannot fall
out of sync. The limitation: the scene graph must evolve to express every
semantic relationship that assistive technology needs.

---

## Platform & Hardware

**Our approach:** arm64 exclusively (Apple Silicon). Runs in a custom hypervisor
on macOS that provides Metal GPU passthrough, virtio-blk/input/console/9p
devices, DTB-based device discovery, and built-in screenshot capture for
deterministic visual testing. Bare-metal hardware is a future target.

|                  | This OS                        | Linux                       | macOS                     | Fuchsia                  | Redox                  |
| ---------------- | ------------------------------ | --------------------------- | ------------------------- | ------------------------ | ---------------------- |
| Architectures    | arm64                          | x86, arm64, RISC-V, ...     | arm64 (+ x86 via Rosetta) | arm64, x86               | x86 primarily          |
| Hardware drivers | Virtio (hypervisor-mediated)   | Thousands (native)          | Apple hardware (native)   | Google hardware + virtio | QEMU + limited native  |
| GPU              | Metal (hypervisor passthrough) | Mesa (Vulkan/GL, many GPUs) | Metal (Apple GPUs)        | Vulkan                   | Software + limited GPU |
| Boot             | DTB from hypervisor            | UEFI, BIOS, devicetree      | iBoot (Apple proprietary) | Bootloader               | UEFI                   |
| Displays         | Single                         | Multi-monitor               | Multi-monitor             | Multi-display            | Single                 |

**Tradeoffs:** The hypervisor provides a controlled development environment —
deterministic screenshots, event injection, crash capture — that bare-metal
cannot. Virtio is a standard device model shared with QEMU and most hypervisors,
so driver work transfers. Metal passthrough gives native GPU performance and
real font rendering quality, but locks the render backend to Apple hardware. A
Vulkan or software backend would be needed for portability — both would slot
behind the existing render interface. Single architecture is an acknowledged
limitation; the kernel's arm64-specific code (boot, exception handling, page
tables) would need porting for other ISAs.

---

## Networking & Ecosystem

No networking yet. No package manager. No application ecosystem. All code is
first-party Rust. The OS communicates with the host only via virtio-9p (shared
filesystem) and virtio-console (serial).

This is the most obvious gap relative to any system that ships to users. It is a
deliberate sequencing choice: the document model, rendering pipeline, and
editing architecture are designed to be correct independent of networking. The
network stack will plug in behind existing interfaces when it arrives — it does
not reshape the architecture. But until then, the OS is an island.

---

## Build System & Developer Experience

**Our approach:** Cargo workspace with kernel and userspace crates.
`cargo build -p kernel` cross-compiles the kernel to `aarch64-unknown-none`;
`cargo test -p kernel --lib` runs 558 host-side tests on the build machine. A
separate `user/integration-tests` crate builds bare-metal init binaries for
hypervisor boot. Makefile provides verification targets: `make test`,
`make miri`, `make fuzz`, `make bench-check`, `make nightly`. Configuration via
`kernel/src/config.rs` (capacity limits, page size) and `.cargo/config.toml`
(target features: LSE atomics, RCPC).

|                     | This OS                                  | Linux                          | Fuchsia                  | Redox                   | Haiku                    |
| ------------------- | ---------------------------------------- | ------------------------------ | ------------------------ | ----------------------- | ------------------------ |
| Build system        | Cargo workspace + Makefile               | Kbuild + Make + Kconfig        | GN + Ninja + fx          | Make + Cargo            | Jam (custom)             |
| Config management   | Rust const files                         | .config (thousands of options) | GN args + product config | Config.toml + Makefiles | Various                  |
| Cross-compilation   | Native (Rust target triple)              | Cross-compiler toolchain       | Built-in (fx set)        | Built-in                | Cross-compiler toolchain |
| Build time (clean)  | ~5s (kernel only)                        | Minutes to hours               | Minutes to hours         | Minutes                 | Minutes to hours         |
| Verification gates  | Pre-commit (clippy+test+build) + nightly | Scattered CI configs           | CQ integration           | GitHub Actions          | BuildBot                 |
| Incremental rebuild | Cargo-managed                            | Incremental by design          | Ninja incremental        | Make incremental        | Jam incremental          |

**Tradeoffs:** Rust nightly required for `no_std` features and the
`aarch64-unknown-none` target triple. No configuration system beyond Rust's
`cfg` — no kernel .config, no feature flags. For a single-target, single-user
system this is simplicity; for a multi-platform OS it would be insufficient.

---

## Testing & Verification

**Our approach:** The kernel underwent a 12-phase verification campaign
(`design/kernel-verification-plan.md`) applying every technique short of formal
verification. Host-side tests (558) cover unit, syscall-level, property-based,
pipeline, and verification scenarios. 4 structured fuzz targets with
invariant-checking harnesses. 33 property tests (proptest) for state machine
properties. Miri for undefined behavior detection. Mutation testing to verify
tests detect real bugs. AddressSanitizer for memory errors. SMP bare-metal
stress tests on 4 vCPUs. Per-syscall cycle-accurate benchmarks with statistical
regression thresholds. Debug-build runtime invariant checking (16 structural
invariants verified after every syscall on bare-metal). 20 bugs found and fixed
during the campaign.

|                       | This OS                                          | Linux                       | Fuchsia                   | Redox          | seL4           |
| --------------------- | ------------------------------------------------ | --------------------------- | ------------------------- | -------------- | -------------- |
| Unit tests            | 558 (kernel, host-side)                          | kselftest, kunit            | Extensive (host + device) | Moderate       | Formal proofs  |
| Property tests        | 33 proptests (state machine + boundary values)   | Limited                     | Some                      | No             | N/A            |
| Fuzzing               | 4 targets, invariant-checking, 200K+ runs        | Syzkaller (extensive)       | CQ fuzzing                | Limited        | N/A            |
| UB detection          | Miri (host tests) + ASan                         | KASAN, KMSAN, KCSAN         | ASan, MSan                | No             | N/A            |
| Mutation testing      | cargo-mutants on all critical files              | Limited                     | No                        | No             | N/A            |
| Integration tests     | 34 bare-metal tests (hypervisor boot)            | kselftest, LTP              | CQ bots, emulator         | QEMU boot      | Proof-based    |
| SMP stress            | 4-vCPU bare-metal: IPC, lifecycle, event stress  | Extensive                   | CQ stress                 | Limited        | N/A            |
| Performance baselines | Per-syscall cycle-accurate, P99 + 3σ thresholds  | perf, eBPF                  | Tracing                   | No             | N/A            |
| Runtime invariants    | 16 checks after every syscall (debug bare-metal) | CONFIG*DEBUG*\* options     | Debug checks              | No             | Proved correct |
| Fault injection       | OOM at every allocation point + capacity exhaust | Fault injection framework   | Various                   | No             | N/A            |
| CI                    | Local (pre-commit + nightly gates)               | Extensive (0-day, kernelci) | Extensive (CQ, CI/CD)     | GitHub Actions | Various        |

**Where this is unusual:** The 12-phase verification campaign is more thorough
than most non-formally-verified kernels. The combination of property testing,
structured fuzzing with invariant checking, mutation testing, Miri, and
debug-build runtime invariant checking on bare-metal creates overlapping
detection layers — each technique catches bugs the others miss. The bug
discovery curve (17 bugs in sessions 1-2, 3 trickle bugs in sessions 8-10, zero
in session 11) suggests diminishing returns on isolated verification.

**Tradeoffs:** No CI server. All testing is local. For a single-developer
project this works; for collaboration it would be a blocker. Not formally
verified — every technique is probabilistic, not a proof. The host-side tests
run with `--test-threads=1` due to shared kernel state. Visual regression
testing (planned: 15 specs, `verify.py`) will be added when the userspace
rendering pipeline is in place.

---

## Documentation

**Our approach:** 14 design documents (~6,200 lines) covering philosophy,
foundations, decision register (17 decisions with tiers, tradeoffs, dependency
chains, and implementation readiness), architecture narrative, roadmap,
glossary, and journal. Kernel design notes (~1,500 lines) document the rationale
for every subsystem. Userspace design notes document every library and service
with status (foundational / scaffolding / demo). Five Mermaid diagrams visualize
architecture, decisions, dependencies, rendering pipeline, and component
relationships. A CLAUDE.md file in every directory provides AI-readable project
context.

|                         | This OS                                                           | Linux                                            | Fuchsia                              | Redox                   | seL4                             |
| ----------------------- | ----------------------------------------------------------------- | ------------------------------------------------ | ------------------------------------ | ----------------------- | -------------------------------- |
| Design rationale        | Extensive (decision register, dependency chains, tradeoff tables) | Scattered (Documentation/, commit messages, LWN) | Extensive (RFC process, design docs) | Moderate (README, book) | Extensive (formal specs, papers) |
| Architecture docs       | Narrative + diagrams + decision map                               | Documentation/process, Documentation/arch        | Architecture guides                  | Wiki                    | Academic papers                  |
| Per-subsystem rationale | Kernel DESIGN.md + userspace DESIGN.md                            | Per-subsystem docs (varying quality)             | Extensive                            | Limited                 | Formal proofs serve as docs      |
| Glossary                | Formal (layered definitions, displaced terms)                     | Informal                                         | Formal                               | Informal                | Formal (specification)           |
| Decision tracking       | 17 decisions with tiers, confidence, reversibility, blast radius  | None (organic)                                   | RFCs                                 | None                    | N/A                              |

**Where this is unusual:** Most OS projects document _what_ the code does. This
project documents _why_ each decision was made, what alternatives were
considered and rejected, what the blast radius would be if the decision were
revisited, and which decisions depend on which others. The decision register
tracks confidence levels and reversibility for each choice. This is more typical
of architecture decision records in enterprise software than hobby OS projects.

**Tradeoffs:** Thorough documentation costs time and creates a maintenance
burden — if the code diverges from the docs, the docs become misleading rather
than helpful. The per-directory CLAUDE.md files are designed for AI-assisted
development (Claude Code) and may not be useful to human readers who don't use
that toolchain. No generated API documentation (no `rustdoc` published), since
the codebase is `no_std` with no public crate interface.

---

## Maturity & Scope

| Dimension             | This OS                    | Fuchsia                 | Redox                   | Haiku                   | seL4                     |
| --------------------- | -------------------------- | ----------------------- | ----------------------- | ----------------------- | ------------------------ |
| Active development    | ~4 months                  | ~10 years               | ~11 years               | ~23 years               | ~17 years                |
| Codebase              | ~90K LOC Rust (29K+61K)    | Millions LOC (C++/Rust) | ~400K LOC Rust          | Millions LOC (C++)      | ~10K LOC C (kernel)      |
| Tests                 | 558 + 4 fuzz + 33 proptest | Extensive               | Moderate                | Extensive               | Formal proofs + tests    |
| Content types handled | 3 (text, rich text, PNG)   | N/A (delegated to apps) | N/A (delegated to apps) | N/A (delegated to apps) | N/A                      |
| Self-hosting          | Not a goal                 | Partial                 | In progress             | Yes                     | Not a goal               |
| Contributors          | 1                          | ~500+                   | ~80+                    | ~100+                   | ~30+                     |
| Ships to users        | No                         | Yes (Nest Hub, Pixel)   | Alpha ISOs              | Beta releases           | Deployed (defense, auto) |

**Honest assessment:** The kernel has been rewritten from first principles and
verified through a 12-phase campaign (20 bugs found and fixed, zero remaining
after convergence). Userspace is in active development: service infrastructure,
drivers (console, virtio-input/blk, Metal GPU render), core libraries, the full
document pipeline (document, layout, presenter services), and three content
types (text/plain, text/rich, image/png) are implemented with visual
verification.

The comparison here is not "this OS vs production systems" — it's "the design
choices this OS makes vs the design choices production systems make, and what
the tradeoffs imply." The kernel is production-grade in verification depth; the
system as a whole is pre-alpha with a working rendering pipeline.

---

## Summary

### Genuine strengths (by design, not maturity)

- **OS-level undo** — No other OS provides per-document undo as a system
  primitive. COW snapshots at operation boundaries, cross-editor,
  crash-resilient.
- **Crash-isolated editing** — Editor process crashes do not lose document
  state. The OS owns the data; the editor is a replaceable tool.
- **Visual consistency** — One renderer means identical typography, color,
  effects, and accessibility across the entire system. No app gets it wrong.
- **Accessibility by construction** — Semantic metadata lives in the same scene
  graph the renderer uses. No parallel tree to maintain or lose sync.
- **Content model coherence** — Mimetypes flow from filesystem through document
  store through rendering pipeline through accessibility. One concept, end to
  end.
- **Editor portability** — Switch editors without migrating files, losing undo
  history, or changing format.
- **Architectural documentation** — The design register, decision rationale, and
  dependency chains are unusually thorough for a project at this stage.

### Genuine weaknesses

- **No ecosystem** — No existing software runs. No compatibility layer is
  planned. Every tool must be written from scratch in `no_std` Rust.
- **Hardware support** — One architecture, one GPU vendor, mediated by a
  hypervisor. No native hardware drivers.
- **Maturity** — ~4 months old, single developer. The kernel is well-verified
  (12-phase campaign); the userspace is in active development with a working
  rendering pipeline.
- **Networking** — None.
- **Content breadth** — Three content types (text/plain, text/rich, image/png)
  with full rendering pipeline. Each additional type requires a decoder (leaf
  node) and layout handler.
- **Rendering constraints** — Applications that need custom visual output
  (games, CAD, video editing, data visualization) have no path today.
- **No multi-user, no multi-display** — Single-user, single-screen by design
  scope.

### Deliberate tradeoffs

| Chose                    | Over                                 | Because                                                                                               |
| ------------------------ | ------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| Document-centric model   | App-centric compatibility            | Enables OS-level undo, crash recovery, editor portability, consistent accessibility                   |
| No POSIX                 | Existing software ecosystem          | POSIX abstractions (fd, pipe, fork, signals) don't map to documents, tools, and capabilities          |
| OS-controlled rendering  | Application rendering freedom        | Consistent visuals, accessibility by construction, single optimization target                         |
| From-scratch microkernel | Linux / Zircon / seL4                | Clean syscall design, full understanding, learning goal; behind a stable syscall interface if swapped |
| Rust everywhere          | C/C++ (more drivers, more libraries) | Memory safety kernel-through-userspace; `no_std` forces minimalism                                    |
| COW filesystem           | Journaling (simpler, less overhead)  | Per-document snapshots unlock OS-level undo                                                           |
| Metal GPU passthrough    | Software rendering (portable)        | Native GPU performance, real font rendering quality, deterministic visual testing                     |
| Fixed-size IPC messages  | Variable-size (more flexible)        | Cache-friendly, simple ring buffer, no allocation on IPC hot path                                     |
| Two IPC mechanisms       | One mechanism for everything         | Discrete events and continuous state have different optimal transports                                |

### Open questions

Areas where the design has not committed, and where the answer will
significantly affect how the OS compares:

- **Layout engine** (#15) — How compound documents compose spatially. No design
  yet.
- **Interaction model** (#17) — How users navigate between documents, invoke
  commands, manage workspaces. Exploring.
- **View state** (#10) — How per-document view state (scroll position, zoom,
  selection) persists. Leaning toward opaque blobs.
- **Custom rendering** — Can the scene graph evolve to handle content types that
  need non-standard visual output? Unknown pressure point.
- **Scalability** — COW snapshots, scene graph compilation, and single-renderer
  architecture are untested with large documents or many concurrent documents.
- **Live queries** — Designed for (following BeOS), not yet implemented.
  Required for the query-based navigation model to feel responsive.
