# Exploration Journal

A research notebook for the OS design project. Tracks open threads, discussion backlog, and insights across sessions. This is the "pick up where you left off" document.

---

## Milestone Roadmap (2026-03-26)

**Status: AGREED.** See `roadmap.md` for the canonical milestone plan (v0.5–v1.0), sequencing rationale, and decision dependencies. Revised after completing v0.3 (rendering) and v0.4 (persistence).

---

## v0.4 Document Store Design (2026-03-26)

**Status: ALL PHASES COMPLETE (A–G).** v0.4 is done.

### Sprint scope

v0.4 completes "The Document Store" — after this sprint, every piece of persistent content in the OS has identity (FileId), type (media type), metadata (queryable), and history (snapshots). Five work items, foundation-up:

1. **Phase C:** Metadata in fs library + store library + mount-on-reboot ✓
2. **Phase D:** Factory disk image builder + richer IPC protocol ✓
3. **Phase E:** Replace 9p boot path — fonts from native fs ✓
4. **Phase F:** Multi-document persistence in core ✓
5. **Phase G:** Undo/redo via COW snapshots wired to core ✓

### Two-library architecture

The persistence layer is two separate libraries with a clean boundary:

```text
┌───────────────────────────────────────────────┐
│  Store  (libraries/store/)                    │  ← Public API. Metadata layer.
│  - create(mimetype) → FileId                  │
│  - set/get attributes (string key-value)      │
│  - query(filter) → Vec<FileId>                │
│  - read/write content (delegates to fs)       │
│  - snapshot/restore (delegates to fs)         │
│  - commit (writes catalog + delegates to fs)  │
│  Uses Box<dyn Files> — no generic parameter.  │
├───────────────────────────────────────────────┤
│  FS  (libraries/fs/)                          │  ← Generic COW filesystem.
│  - BlockDevice trait, COW, inodes, allocator  │
│  - Snapshots, crash consistency               │
│  - Stores bytes. Knows nothing about content. │
│  Reusable by anyone, not document-specific.   │
└───────────────────────────────────────────────┘
```

**Why separate:** The fs library is generic infrastructure (useful to anyone). The store library adds document-centric semantics specific to this OS. Each can be swapped independently. The store library uses `Box<dyn Files>` (trait object) so it never sees `BlockDevice` or `Filesystem<D>` — the generic parameter is fully contained in the fs library and the filesystem service.

**Consumers:** Only the filesystem service touches the store library directly. Core and everything above talk to the filesystem service via IPC. The factory disk image builder uses the same store library on the host.

### Catalog design

A single **catalog file** stored in the filesystem holds all metadata for all documents. The catalog is "just a file" — the fs library doesn't know it's special.

- **Discovery:** `Files` trait gains `set_root(FileId)` and `root() -> Option<FileId>`. The store calls `set_root(catalog_id)` during init. On open, `root()` returns the catalog FileId in O(1). This is a generic filesystem concept (like root inode in ext4), not document-specific.
- **Schema:** Each entry stores `file_id` (u64), `media_type` (string, mandatory), and optional key-value attributes (string→string). Size/created/modified come from the fs library's `FileMetadata` — no duplication.
- **Serialization:** Variable-length entries, sequentially packed. Loaded entirely into `BTreeMap<FileId, CatalogEntry>` at mount. Written entirely on commit. Same pattern as inode table and snapshot store.
- **Snapshots include the catalog.** When the store creates a snapshot, it transparently includes the catalog file. Restoring a snapshot restores metadata in sync with content.

### Files trait additions

```rust
fn list_files(&self) -> Result<Vec<FileId>, FsError>;     // needed for queries
fn set_root(&mut self, file: FileId) -> Result<(), FsError>;  // catalog discovery
fn root(&self) -> Option<FileId>;                           // catalog discovery
```

### Query model

```rust
pub enum Query {
    MediaType(String),                         // exact: "font/ttf"
    Type(String),                              // category: "font" matches font/*
    Attribute { key: String, value: String },   // exact key=value
    And(Vec<Query>),
    Or(Vec<Query>),
}
```

Uses standard MIME terminology: **type** = top-level category (font, image, text), **subtype** = specific format (ttf, png, plain), **media type** = full string (font/ttf).

### Inode table scaling

The inode table (FileId → inode block mapping) moves from single-block to **linked-block chain** (same pattern as snapshot store). Removes the 1365-file hard limit.

**Accepted tradeoff:** The entire inode table is loaded into memory at mount and rewritten on commit. This is O(n) in file count — acceptable at personal-OS scale (hundreds to low thousands of files). A COW B-tree would be needed for general-purpose scale. The `Files` trait boundary allows internals to be replaced without changing consumers. This tradeoff will be documented in the fs library.

### "Everything is a file" — reframed

Original premise: "everything is a file" (Unix-style). Revised: **"all persistent content with a media type is a file."** The litmus test: does it have a media type and does it persist? If yes, it belongs in the filesystem. If no (scene graph, IPC channels, process state), it doesn't.

This keeps the filesystem meaningful as the persistent content store without forcing runtime state through it for philosophical purity.

### Factory disk image

A build-time host tool creates a pre-populated disk image using `Store<Filesystem<FileBackedDevice>>`. Contents: system fonts (font/ttf), test image (image/png), catalog with metadata. The guest boots, mounts, queries for fonts by type. No "first boot" special path. 9p becomes unnecessary for boot (may survive as a dev/import tool or be removed).

### Design decisions

- **No file paths.** Files have FileId + metadata. Users find files by query, not by path. There are no paths in this system — not even as metadata.
- **Media type is mandatory at creation.** `store.create("font/ttf")` — you cannot create a file without declaring its type.
- **Preparation required for any fs swap.** Swapping the underlying filesystem always requires building a catalog. The `set_root`/`root` addition is trivially implementable on any fs.
- **Foundation-up sequencing.** Build fs changes → store library → IPC protocol → factory image → boot sequence → multi-document → undo/redo. No layer-switching.

### Services as translation layers (architectural insight)

Every service in this OS is a **translation layer** between core and a subsystem that core shouldn't know about. The render service translates scene graph → GPU commands. The document service translates document operations → disk I/O. The input service translates hardware events → IPC messages. Core never touches hardware or low-level primitives.

This framing clarifies: "leaf node" is relative to your system boundary. The render service is a leaf of our OS, but from a wider view it's just another translator between software and hardware (and the monitor is a translator between hardware and the user's eyes). It's translation layers all the way down — our boundary is just where we stop being responsible.

This also explains why the IPC protocol for v0.4 stays minimal scaffolding: core's architecture hasn't been deliberately designed yet. Investing in a rich document-management IPC protocol now would design core by accident, encoding premature UX assumptions in plumbing.

### Design extracted to permanent docs

The durable content from the v0.4 sprint spec has been extracted into `design/foundations.md` (§Persistence Architecture — two-library design, catalog, scale analysis, atomic writes, services as translation layers) and `design/decisions.md` (#16 Tech Foundation — COW filesystem and document store settled sub-decisions).

---

## Filesystem Bare-Metal Integration — Phase B Complete (2026-03-26)

**Status: COMPLETE** — All 4 steps (B1–B4) implemented. COW filesystem running as a bare-metal userspace service, persisting document edits to disk.

### What was built

| Step | What                                                                                                                                                                                                   |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| B1   | virtio-blk driver rewrite: `BlkDevice` struct with `read_block`/`write_block`/`flush`, `VIRTIO_BLK_F_FLUSH` feature negotiation, self-test (read/write/verify cycle)                                   |
| B2   | Hypervisor file-backed virtio-blk backend: `VirtioBlock.swift`, `--drive` flag, `F_FULLFSYNC` on flush for crash consistency on macOS                                                                  |
| B3   | Filesystem service port to bare-metal: `no_std` fs library at `system/libraries/fs/`, `VirtioBlockDevice` with `RefCell` for interior mutability, filesystem service at `system/services/filesystem/`  |
| B4   | Core integration via IPC: `protocol::blkfs` boundary, core sends `MSG_FS_COMMIT` at operation boundaries (after draining pending editor messages), doc buffer shared read-only with filesystem service |

### Design decisions that emerged

- **`VirtioBlockDevice` uses `RefCell` for interior mutability.** `BlockDevice::read_block` takes `&self` (callers shouldn't need `&mut` to read), but virtio I/O mutates internal queue state. `RefCell` provides runtime borrow checking without requiring `&mut` throughout the call chain.
- **`HashMap` → `BTreeMap` for `no_std` port.** The host prototype used `HashMap`; the bare-metal port uses `BTreeMap` (available in `alloc`, no hasher dependency).
- **Filesystem service deferred startup in init.** `memory_share` requires the target process to be created but not yet started. Init creates the filesystem process, shares the doc buffer read-only, then starts it. This is Phase 10 in init's boot sequence.
- **Core sends `MSG_FS_COMMIT` at operation boundaries.** After draining all pending editor messages (insertions, deletions), core signals the filesystem service to snapshot. The filesystem reads the doc buffer from shared memory and writes + commits to disk.
- **Doc buffer shared read-only between core and filesystem.** No data copying — the filesystem service reads the same shared memory page that core writes to. Core is the sole writer; both filesystem and editor are read-only consumers.

### What remains (deferred to future milestones)

- **Timestamps:** Inode timestamps not yet populated with real time values.
- **Snapshot cleanup:** Orphaned snapshots (from truncated redo history) are not deleted from disk.
- **Operation coalescing:** Undo is currently per-keystroke. Coalescing rapid edits into single undo steps is a future enhancement.

---

## Filesystem Host Prototype — Phase A Complete (2026-03-25)

**Status: COMPLETE** — 4,312 lines of Rust, 133 tests, zero warnings. All 7 steps (A1–A7) implemented.

### What was built

A complete COW filesystem running as a regular Rust binary on macOS. All layers from raw blocks to the `Files` trait:

| Step | Module             | What                                                                                                                                                                          |
| ---- | ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1   | `block.rs`         | `BlockDevice` trait + File/Memory/Logging implementations. Logging device records writes with flush epochs for crash testing.                                                 |
| A2   | `superblock.rs`    | 16-slot ring at disk start. CRC32 per entry. Highest valid txg wins on mount. Format/mount/commit.                                                                            |
| A3   | `alloc.rs`         | Sorted free-extent list with coalescing. First-fit allocation. Persisted as single COW block. Self-allocating (turtles solved). Proptest: random alloc/free sequences.        |
| A4   | `inode.rs`         | One 16 KiB block per inode. 64-byte header + 16×12-byte extent list + 16128-byte inline data region. u48 birth_txg on extents.                                                |
| A5   | `filesystem.rs`    | COW write path (entire-file COW for extent-based). Two-flush commit protocol. 2-generation deferred block reuse. Inode table as single COW block.                             |
| A6   | `snapshot.rs` + fs | Multi-file snapshots. Linked-block persistence. Birth-time deletion (O(n²) reference check, Model D optimization deferred to stress testing). Shared block safety on restore. |
| A7   | `lib.rs` + fs      | `Files` trait with `FileId`/`SnapshotId` newtypes. Object-safe (`dyn Files` works). Full lifecycle tested through trait.                                                      |

### Design decisions that held

All tentative decisions from the design session survived implementation without modification:

- Flat namespace (FileId → inode block)
- 16 KiB blocks + inline data
- Pure COW crash consistency
- Superblock ring (16 entries)
- Sorted free-extent list allocator
- Per-file snapshot chains with multi-file grouping
- `Files` trait with explicit `commit()`

### Design decisions that emerged during implementation

- **COW-entire-file** for extent-based writes. O(file_size) per write instead of O(delta). Acceptable for document workloads (files < 1 MiB). Keeps extent lists compact (one entry per write generation). Per-block COW is a future optimization.
- **Inode table as a single COW block.** Maps FileId → inode block. Max 1365 entries per 16 KiB block. Sufficient for a personal OS.
- **Snapshot store as linked blocks.** Serialized blob across a chain of blocks with next-pointers. Grows as snapshot data accumulates. One block suffices for typical use.
- **Deferred free timing:** blocks freed at txg D reusable when `D <= current_txg - 1` at commit start. Verified: 2-generation gap between free and reuse.

### What's next: Phase B — COMPLETE

Phase B (bare-metal integration) is complete. See "Filesystem Bare-Metal Integration — Phase B Complete (2026-03-26)" above.

### Deferred to stress testing (build plan layer 3+)

- Birth-time optimization for snapshot deletion (currently O(n²) reference check)
- Crash consistency tests via `LoggingBlockDevice` replay
- proptest-state-machine model-based tests for full filesystem
- Per-block COW optimization
- Leaked block recovery after crash (fsck scan)

---

## Codebase QoL Backlog (2026-03-25)

**Status: BACKLOG** — not blocking; revisit when touching adjacent code.

Findings from a structural audit at 115K lines / 211 .rs files / 2,236 tests.

### 1. metal-render/main.rs — 3,499-line monolith

Largest non-test file. virgil-render was already split into 12 files for the same complexity reasons. Split when next modifying this service (not needed for v0.4 filesystem work).

### 2. Test subsetting

69 test files in flat `test/tests/` directory. No way to run just kernel tests or just rendering tests without grep-filtering. As the filesystem adds more tests, being able to run subsets would save iteration time. Options: naming prefix convention + shell aliases, or `#[cfg_attr]` test groups.

### 3. sys/lib.rs — growing monolith (904 lines)

Syscall wrappers, GlobalAlloc, spinlocks, timers, atomics all in one file. Every userspace program depends on this. Split by responsibility (syscalls, allocator, sync primitives) when the filesystem service adds new syscall wrappers.

### Not worth acting on

- **Build system** (560 lines) — clean, focused, no change needed.
- **Protocol library** — well-split by transport, scales cleanly.
- **Service boilerplate duplication** — intentional for isolation.
- **animation/lib.rs** (1,514 lines) — small enough, stable.
- **Slab size classes** — no evidence of kernel objects >2K needing them.

---

## PAGE_SIZE Single Source of Truth (2026-03-25)

**Status: DONE** — implemented.

`system/system_config.rs` is the SSOT for 9 root constants (PAGE_SIZE, PAGE_SHIFT, RAM_START, KERNEL_VA_OFFSET, USER_CODE_BASE, CHANNEL_SHM_BASE, USER_STACK_TOP, USER_STACK_PAGES, SHARED_MEMORY_BASE). All `u64`. Consumed by:

- **Kernel:** `paging.rs` includes via `mod system_config { include!(...) }; pub use system_config::*`
- **Userspace libraries:** ipc, sys, protocol, virtio — each includes in a private `mod system_config`
- **Linker scripts:** `.ld.in` templates with `@PLACEHOLDER@` substitution, generated by `build.rs`
- **Tests:** `test/build.rs` + `ipc/build.rs` + `protocol/build.rs` provide the env var for Cargo builds
- **boot.S:** keeps manual `.equ` values; `const_assert!` in `paging.rs` catches drift

Env var plumbing: `build.rs` does `std::env::set_var("SYSTEM_CONFIG", path)` for manual rustc invocations + `cargo:rustc-env=SYSTEM_CONFIG=...` for the kernel crate. Library and test crate `build.rs` files emit `cargo:rustc-env` for the Cargo dependency path.

Lessons learned during implementation:

- `//!` inner doc comments fail in `include!`'d files (not at module top) — use `//` instead
- `#[allow(dead_code)]` on `include!()` is silently ignored — wrap in `mod { #![allow(dead_code)] include!(...) }` instead

---

## Filesystem Design — Layer Map and Key Decisions (2026-03-25)

**Status: Phase A COMPLETE** — host prototype implemented and tested. 4,312 lines, 133 tests. Phase B (bare-metal integration) next.

### Context

v0.3 is complete (2,236 tests). v0.4 focuses on the filesystem — the load-bearing piece beneath undo, clipboard, metadata queries, and the full document lifecycle. This entry records the design space exploration.

### The filesystem stack

Seven layers between raw disk blocks and the `Files` trait that the OS service calls:

```text
┌──────────────────────────────────────────────┐
│  Layer 6: FILES API                          │  The Files trait (already designed)
│  Translates semantic ops → filesystem ops    │
├──────────────────────────────────────────────┤
│  Layer 5: SNAPSHOT ENGINE                    │  Per-file + multi-file snapshots
│  Create, restore, delete, prune              │
├──────────────────────────────────────────────┤
│  Layer 4: NAMESPACE                          │  FileId → inode (flat, no directories)
├──────────────────────────────────────────────┤
│  Layer 3: INODE / METADATA                   │  Size, timestamps, block pointers
│  No mimetype (that's core's metadata DB)     │
├──────────────────────────────────────────────┤
│  Layer 2: BLOCK ALLOCATOR                    │  Free-space tracking, COW allocation
├──────────────────────────────────────────────┤
│  Layer 1: TRANSACTION / JOURNAL              │  Crash consistency via pure COW
├──────────────────────────────────────────────┤
│  Layer 0: BLOCK I/O                          │  virtio-blk (hypervisor only)
└──────────────────────────────────────────────┘
```

### Decision: No mmap (TENTATIVE)

The original design assumed kernel-mediated memory mapping of files (mmap). After analysis, **explicit reads via IPC** is the better fit for this architecture:

**Why not mmap:**

- Requires a kernel pager — massive complexity (page fault handling, page cache, FS↔kernel coupling, eviction policy). One of the most complex parts of a monolithic kernel.
- Sole-writer architecture already means editors send write operations through core via IPC. mmap only helps the read path.
- Documents are bounded in size (text: <1 MiB, images decoded: <50 MiB). Demand paging shines for huge files (databases); for documents, load whole.
- Video/audio need explicit streaming regardless — mmap doesn't solve it. The scene graph's `Content::Stream` (frame ring in Content Region) is the streaming design.
- The current architecture already does this: core loads file bytes into shared memory, services get read-only mappings. Extending a proven pattern, not building a new one.

**What this means:**

- The kernel stays simple — no file-aware page fault handling, no page cache
- The filesystem service is fully decoupled from the kernel (pure userspace)
- Core calls `Files::read()` / `Files::write()` / `Files::snapshot()` over IPC
- Core loads file content into Content Region shared memory; editors read from there
- Upper bound on "load whole" is hundreds of MiB — covers all document types in scope
- For large media (video): explicit streaming via Content Region frame ring

### Decision: Flat namespace (TENTATIVE)

No directories. FileId(u64) → inode. No path resolution, no directory tree, no rename mechanics.

**Why:** The design (Decision #7) says "users navigate by query, not path." Paths are metadata, not the organizing principle. The `Files` trait already uses FileId, not paths. Directories exist in traditional FSes because paths WERE the organizing principle. We've rejected that. A flat namespace is the honest implementation.

**Implications:**

- Enumeration requires the metadata query system (no `ls /dir/`)
- Compound documents reference parts by FileId (works naturally)
- Debugging requires tooling (query the metadata DB, not browse the raw disk)
- No natural access control grouping (fine — single-user OS)
- Allocation locality for related files (compound doc parts) handled by allocator policy

### Decision: Mimetype not in filesystem (TENTATIVE)

The filesystem stores bytes, manages blocks, handles snapshots. It does not understand content types. Mimetype belongs in core's metadata layer.

**The argument:** Mimetype describes how bytes should be interpreted — that's content understanding, which the architecture places in core (OS service), not in the storage layer. The filesystem is to mimetype as the compositor is to PNG: wrong layer.

**How it works:**

- At ingest, core determines mimetype from original filename extension + magic bytes + explicit declaration
- Core stores mimetype in the metadata DB (alongside original filename, user tags, relationships)
- The metadata DB lives ON the filesystem, getting COW/snapshot protection for free
- If metadata DB is lost, core reconstructs from content detection (degraded but functional)
- Mimetype is cached in the metadata DB (not re-derived on every access)

**The inode contains only:** FileId, size, timestamps (created/modified), block pointers. That's it.

**Type hierarchy:** One primary mimetype per file. The type hierarchy (e.g., `text/markdown` degrades to `text/plain`) is a system property in core's type registry, not per-file metadata. The filesystem doesn't participate in type dispatch.

### Decision: Filesystem is a userspace service (TENTATIVE)

Not in-kernel (complex code stays out of TCB per microkernel philosophy). Not part of core (core understands documents, not blocks). A separate userspace service that core communicates with via IPC.

```text
Core (documents) ──IPC──→ FS service (blocks, inodes, snapshots) ──virtio──→ disk
```

Performance argument for in-kernel is weak: the hot path in the no-mmap model is shared memory reads (memory speed), not filesystem operations. FS operations (open, snapshot, writeback) are infrequent and tolerate IPC overhead.

### Decision: 16 KiB kernel page size (LEANING)

Current: 4 KiB (boot.S TG0/TG1). Should be 16 KiB to match Apple Silicon:

- macOS/iOS use 16 KiB pages
- Apple memory controller optimized for 16 KiB
- 4× TLB coverage per entry
- Matches the host hardware the hypervisor runs on

**Impact:** Kernel change — boot.S TCR, all page table code, stack sizes, slab allocator, buddy allocator. Worth doing before building the filesystem.

**Independent of block size** in the no-mmap model — page size is kernel memory management, block size is filesystem on-disk format.

### Decision: Block size (LEANING toward 16 KiB + inline data)

Without mmap, block size is purely a filesystem on-disk concern. Options considered:

- Fixed 4 KiB: low waste, high metadata overhead
- Fixed 16 KiB: aligned with page size, higher waste on small files
- Variable (ZFS-style): optimal but complex
- **16 KiB + inline data:** Small files (<~1 KiB) stored directly in the inode. Everything else uses 16 KiB blocks. Simple, aligned, minimal waste for document workloads.

### Decision: Pure COW for crash consistency (LEANING)

Never overwrite a block in place. Write new data to new location, atomically update root pointer. Crash consistency is a structural property, not an additional mechanism. Natural fit: old blocks ARE the undo history. A superblock ring (multiple root pointers) provides extra crash safety for near-zero cost.

### Observation: Documents ≠ Files (1-to-many)

Per Decision #14, ALL documents (even simple ones) are manifests referencing content files. A plain text document = manifest file + content file. The filesystem sees files; core understands the document→manifest→content relationship.

This surfaces the **multi-file snapshot** question: an edit to a compound document may touch multiple files (content + manifest). Undo must restore all of them atomically. The current `Files::snapshot(FileId)` takes one file — needs to be extended to `snapshot(files: &[FileId]) -> SnapshotId` (or equivalent) so the filesystem can atomically snapshot multiple files as a group. The `Files` trait update is deferred to when we design Layer 5 in detail.

### Decision: Snapshot engine — Model D, birth-time + flat extent lists (TENTATIVE)

Four models evaluated: ZFS-style (birth-time + dead lists), Btrfs-style (refcount COW B-trees), Bcachefs-style (key-level versioning), and a hybrid. Model D selected:

**Model D: Birth-time + flat per-file extent lists.** Observation: files are documents — small enough that their extent lists are compact (1–16 entries). No B-tree per file needed.

- Each inode stores an extent list: `[(start_block, count, birth_txg), ...]`
- Small files: inline data in the inode (no extents needed)
- Medium files: extent list in the inode directly (up to 8–16 extents)
- Large files: extent list overflows to a separate block (indirect)
- **Snapshot** = save the current extent list. Extent lists are small → cheap copy.
- **Restore** = swap current extent list with snapshot's.
- **Delete** = walk snapshot's extent list. If extent's `birth_txg > previous_snapshot_txg`, free those blocks. Otherwise, referenced by older snapshot.
- **Multi-file** = save N extent lists in one transaction.

**Why D over A/B/C:**

- vs ZFS (A): Same birth-time insight but without dead list complexity. Extent lists are small enough to walk directly (ZFS needs dead lists because trees can be huge).
- vs Btrfs (B): Avoids unpredictable cascading refcount decrements on delete. High-frequency snapshots would stress Btrfs's weakness.
- vs Bcachefs (C): Avoids full B-tree walk on snapshot deletion and "trickiest" space accounting.

**Risk:** Novel combination — no existing FS does exactly this. Needs stress testing before committing. Plan: prototype on host, stress test with simulated editing workloads (thousands of snapshots, random writes, aggressive pruning).

**Pruning policy:** Core's concern, not filesystem's. FS provides create/delete/list. Core decides when to prune (likely: keep last N for undo + logarithmic thinning for cross-session history).

### Decision: Build from scratch, informed by prior art (DECIDED)

No existing filesystem matches the requirements (flat namespace + per-file high-frequency snapshots + Rust no_std + pure COW). Evaluated RedoxFS, ZFS, Btrfs, Bcachefs, littlefs — none close enough to adapt. Building from scratch with:

- ZFS's birth-time insight for snapshot deletion
- RedoxFS's Rust COW transaction model as reference
- Superblock ring for crash safety (from ZFS/RedoxFS)
- Host-side prototype first (file as block device), then port to bare-metal (virtio-blk)

### Decision: 16 KiB page migration — DO FIRST (DECIDED)

Kernel change before filesystem work. Foundation must be correct before building on top. See dedicated section below.

### Decision: Block allocator — sorted free-extent list, COW-persisted (2026-03-25)

**Research:** Evaluated bitmap (ext4), dual B+tree (XFS), free space tree (Btrfs), space maps (ZFS), bucket/segment (F2FS, Bcachefs), per-CPU tree (NOVA), LSM tree (Fxfs). See `design/research/cow-filesystems.md`.

**Decision:** In-memory sorted free-extent list (`Vec<(start_block: u32, count: u32)>` sorted by start), persisted as a single COW block pointed to by the superblock ring entry.

**Why:** At this scale (4 GiB disk = 262,144 blocks), even worst-case fragmentation produces a free list that fits in one 16 KiB block. Space maps (ZFS) solve a problem we don't have (high-frequency allocator metadata I/O across petabyte pools). Bitmaps fight COW (fixed-location in-place updates). This design has no turtles problem — the free list block is allocated from the previous generation's free list.

**Allocation:** First-fit scan. O(n) where n = number of free extents (typically a few hundred). Free = insert + coalesce adjacent entries. Locality bias: prefer blocks near the file's existing extents when possible.

**Crash recovery fallback:** Walk all inodes + snapshot extent lists → build set of used blocks → invert. O(total blocks) but only needed if the persisted free list is corrupted (like NOVA's approach).

### Decision: On-disk inode — one 16 KiB block with inline data (2026-03-25)

**Research:** Evaluated ext4 (256 B fixed), Btrfs (160 B + separate extent items), ZFS dnodes (512 B–16 KiB variable + bonus buffer), RedoxFS (one-block node), NOVA (128 B + per-inode log), Bcachefs (btree entries), CVFS, F2FS inline data. See `design/research/cow-filesystems.md`.

**Decision:** RedoxFS-style one-block inode. Each inode occupies one 16 KiB block, partitioned into three regions:

```text
┌──────────────────────────────────────────────────────────────┐
│  HEADER (64 bytes)                                           │
│  file_id: u64, size: u64, created: u64, modified: u64        │
│  flags: u32, extent_count: u16, snapshot_count: u16          │
│  indirect_block: u32, snapshot_list_block: u32, reserved     │
├──────────────────────────────────────────────────────────────┤
│  EXTENT LIST (up to 16 entries × 12 bytes = 192 bytes)       │
│  Per extent: start_block (u32) + count (u16) +               │
│              birth_txg (u48, 6 bytes)                        │
├──────────────────────────────────────────────────────────────┤
│  SNAPSHOT LIST (inline up to ~8 records)                     │
│  Per snapshot: snapshot_id (u32) + saved extent list pointer │
│  Overflows to linked snapshot blocks via snapshot_list_block │
├──────────────────────────────────────────────────────────────┤
│  INLINE DATA (remaining ~15.5 KiB)                           │
│  When flags.inline_data set: file content stored here        │
│  Otherwise: unused (zero-filled)                             │
└──────────────────────────────────────────────────────────────┘
```

**Why one-block inodes:** Most document files are small (manifests, text, configs < 15 KiB). Inline data means these files are one block on disk — one read gets metadata + content + extent list + snapshot history. Zero indirection. For larger files: 16 extents × 16 KiB blocks = 256 KiB directly addressable; `indirect_block` handles overflow for rare large files.

**Field sizing:** u32 block addresses → 64 TiB addressable (sufficient for personal OS — if not, widen later behind the same inode interface). u16 extent count → 1 GiB contiguous run. u48 birth_txg → 281 trillion transactions.

### Decision: Superblock ring — 16 entries at disk start (2026-03-25)

**Research:** Evaluated ext4 (primary + sparse backups + journal), ZFS uberblock ring (128 × 4 labels = 512 copies), Btrfs (3 mirrors at fixed offsets), RedoxFS header ring (256 entries), F2FS dual checkpoint, LMDB shadow paging (ping-pong). See `design/research/cow-filesystems.md`.

**Decision:** 16-entry ring. Block 0 = disk header (magic, version, geometry). Blocks 1–16 = superblock ring entries. Block 17+ = data/metadata.

Each ring entry (one 16 KiB block):

```text
magic: u64
txg: u64                    // monotonic transaction counter
timestamp: u64              // nanos since epoch
root_inode_table: u32       // block of inode table root
root_free_list: u32         // block of persisted free-extent list
root_snapshot_index: u32    // block of global snapshot→file mapping
total_blocks: u32
used_blocks: u32
next_file_id: u64           // monotonic FileId counter
checksum: u32 (CRC32)
```

**Why 16:** The ring's job is crash safety, not undo. Per-file snapshots handle undo. 16 entries survive 16 consecutive torn commits — beyond that, the disk is physically failing. ZFS uses 128 because it commits every 5–30 seconds and wants hours of rollback; we have explicit snapshots for that. 16 × 16 KiB = 256 KiB overhead.

**Mount:** Scan all 16 entries, pick highest `txg` with valid CRC32.

### Decision: Transaction commit protocol — two-flush with 2-generation deferred reuse (2026-03-25)

**Research:** Evaluated ZFS (three-flush txg commit with 3-generation deferral), Btrfs (COW convergence + superblock write, relies on flush/barrier), RedoxFS (header ring, no explicit flush — latent bug), WAFL (NVRAM-backed), LMDB (ping-pong meta pages). Also researched virtio-blk guarantees, NVMe atomicity, macOS fsync vs F_FULLFSYNC. See `design/research/cow-filesystems.md`.

**Decision:**

```text
Transaction commit sequence:
1. Write all new data blocks (COW'd file content)
2. Write all new metadata blocks (COW'd inodes, free list, snapshot records)
3. FLUSH (virtio-blk VIRTIO_BLK_T_FLUSH)
4. Write superblock ring entry at txg % 16
5. FLUSH
6. Blocks freed in txg N-2 now become reusable
```

**Deferred block reuse (2-generation):** Freed blocks aren't added to the free list until 2 commits later. Even if flush is incomplete or writes are reordered, the previous transaction's blocks survive through one full additional commit cycle. This is structural immunity to write reordering — crash consistency is a property of the data structure, not of the device behaving correctly. (ZFS uses 3-generation deferral for similar reasons.)

**Checksums:** CRC32 on every superblock ring entry and every inode block. Torn writes are detected and the previous valid state is used.

**Critical implementation note:** virtio-blk supports FLUSH (`VIRTIO_BLK_F_FLUSH` feature bit, `VIRTIO_BLK_T_FLUSH` command type). The current virtio-blk driver reads sector 0 only — needs flush support added. On the hypervisor side, the virtio-blk backend's FLUSH handler MUST use `fcntl(F_FULLFSYNC)`, not `fsync()`, because macOS `fsync()` does NOT flush the disk's volatile write cache (Apple documents this explicitly). A 16 KiB write is NOT guaranteed atomic on any of our target devices.

**Core integration:** Core calls `Files::commit()` at `endOperation` boundaries. Between `beginOperation` and `endOperation`, writes accumulate (COW'd blocks not yet committed). This batches compound document edits into one atomic transaction.

### Decision: Snapshot metadata — per-file chain with multi-file grouping (2026-03-25)

**Research:** Evaluated ZFS DSL (dataset-level, birth-time + dead lists), Btrfs (snapshot = new tree root, refcount COW), WAFL (root inode duplication + bit-plane block map), Bcachefs (key-level snapshot IDs), NILFS2 (continuous log-structured checkpoints). See `design/research/cow-filesystems.md`.

**Decision:** Per-file snapshot chains. Each snapshot record stores a saved copy of the file's extent list at the time of creation.

```text
SnapshotRecord {
    snapshot_id: u32,        // global monotonic ID (from superblock)
    txg_at_creation: u48,    // birth-time for freeing logic
    extent_count: u16,
    extents: [Extent],       // copy of the file's extent list at snapshot time
    prev_record_block: u32,  // linked list for overflow
}
```

**Inline vs overflow:** ≤8 snapshots stored in the inode's snapshot region. >8 overflows to linked blocks via `snapshot_list_block` pointer.

**Multi-file atomic snapshots:** A global snapshot index maps `SnapshotId → Vec<(FileId, snapshot_record_block)>`. This is how `restore(SnapshotId)` knows which files to revert. The index itself is a COW object pointed to by the superblock.

**Deletion (Model D birth-time logic):** Walk the snapshot's extent list. For each extent, if `birth_txg > previous_snapshot_txg`, the blocks can be freed (no older snapshot references them). Otherwise, they belong to an older snapshot. This is ZFS's birth-time insight scaled down from tree-level to extent-list-level — viable because document files have compact extent lists (1–16 entries).

### Decision: Files trait — explicit commit, multi-file snapshots (2026-03-25)

**Decision:** The filesystem service exposes this interface to core via IPC:

```rust
trait Files {
    // Lifecycle
    fn create(&mut self) -> FileId;
    fn delete(&mut self, file: FileId) -> Result<(), FsError>;

    // Data access
    fn read(&self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), FsError>;
    fn truncate(&mut self, file: FileId, len: u64) -> Result<(), FsError>;
    fn size(&self, file: FileId) -> Result<u64, FsError>;

    // Snapshots (the undo substrate)
    fn snapshot(&mut self, files: &[FileId]) -> Result<SnapshotId, FsError>;
    fn restore(&mut self, snapshot: SnapshotId) -> Result<(), FsError>;
    fn delete_snapshot(&mut self, snapshot: SnapshotId) -> Result<(), FsError>;
    fn list_snapshots(&self, file: FileId) -> Result<Vec<SnapshotId>, FsError>;

    // Metadata (filesystem-level only — mimetype etc. is core's concern)
    fn metadata(&self, file: FileId) -> Result<FileMetadata, FsError>;

    // Transaction boundary
    fn commit(&mut self) -> Result<(), FsError>;
}
```

**`commit()` is explicit** because core controls transaction boundaries (at `endOperation`). Between begin/end, writes accumulate in memory. Crash mid-operation loses uncommitted writes — correct, because the operation wasn't complete.

**`snapshot(&[FileId])`** because documents are 1-to-many files. Undo must revert all files atomically. The filesystem doesn't know about documents — it just groups files under one SnapshotId.

**`restore(SnapshotId)`** takes only the snapshot ID because the snapshot already records which files it covers (via the global snapshot index). Core doesn't re-specify.

### Stress testing plan (2026-03-25)

**Research:** Evaluated xfstests/fsstress (60+ weighted random ops), ZFS ztest (kill-and-recover with 70% kill rate), CrashMonkey/ACE (OSDI '18, exhaustive crash state exploration), dm-log-writes, SQLite's VFS-based crash simulation (590:1 test-to-code ratio), proptest-state-machine (Rust model-based testing), sled's uncertainty model, deterministic simulation testing (Antithesis/FoundationDB). See `design/research/cow-filesystems.md`.

**Five-layer strategy:**

1. **Property-based unit tests (proptest):** Block allocator (alloc/free sequences, coalescing, no double-free). Extent list operations. Inode serialization round-trips. Birth_txg monotonicity.

2. **Model-based state machine tests (proptest-state-machine):** Reference model = `HashMap<FileId, Vec<u8>>` + snapshot history. Random sequences of 50–200 operations. After every op: reads match reference, block accounting is consistent. Automatic shrinking finds minimal failing sequences.

3. **Crash consistency tests (CrashMonkey + SQLite inspired):** Logging `BlockDevice` wrapper records every write with flush epoch. Replay all prefixes (crash at every write boundary). After each simulated crash: verify superblock validity, block allocation consistency, data is pre-op OR post-op (never hybrid). Uncertainty model: reference accepts either old or new state after crash.

4. **Snapshot regression:** Deletion ordering matrix (oldest-first, newest-first, random, every-other). Space reclamation accounting. Thousands of rapid snapshots. Multi-file atomic snapshot + crash + verify.

5. **Sustained stress (ztest-inspired):** Weighted random ops, minutes to hours. Kill-and-recover cycles. Fragmentation metrics: extent count per file, free extent histogram, largest contiguous free region, sequential read I/O count.

**Implementation order:** Layers 1–2 during host prototype build. Layer 3 immediately after (tests commit protocol). Layers 4–5 once basic FS works end-to-end. The `BlockDevice` trait is the testing seam — design it with fault injection in mind from day one (SQLite's VFS lesson).

### Build order (2026-03-25)

Host prototype first (regular Rust binary on macOS, file-backed block device). Fast compile, full test tooling, proptest, no kernel boot cycles. Everything above `BlockDevice` trait is the same code in both environments.

**Phase A: Host Prototype (Rust, std, macOS)**

| Step | What                                                           | Key deliverable                                   | Test focus                                                             |
| ---- | -------------------------------------------------------------- | ------------------------------------------------- | ---------------------------------------------------------------------- |
| A1   | `BlockDevice` trait + `FileBlockDevice` + `LoggingBlockDevice` | Testing seam, fault injection wrapper             | Unit: read/write/flush round-trip                                      |
| A2   | Superblock ring + disk header                                  | Format, mount, commit, torn write recovery        | Proptest: random crash points via LoggingBlockDevice                   |
| A3   | Block allocator (free-extent list)                             | Alloc, free, coalesce, persist as COW block       | Proptest: random alloc/free, no leaks, no double-free                  |
| A4   | Inode + inline data                                            | Create, read, write inline, serialize/deserialize | Round-trip, inline threshold, extent list ops                          |
| A5   | COW write path + commit protocol                               | Two-flush commit, 2-generation deferred reuse     | Write + commit + read-back, old blocks survive 2 gens                  |
| A6   | Snapshots (Model D)                                            | Create, restore, delete (birth-time), multi-file  | State machine tests (proptest-state-machine), deletion ordering matrix |
| A7   | `Files` trait integration                                      | Full API wired together                           | Crash consistency suite, sustained stress (ztest-style)                |

**Phase B: Bare-Metal Integration**

| Step | What                             | Key deliverable                                                                   |
| ---- | -------------------------------- | --------------------------------------------------------------------------------- |
| B1   | virtio-blk driver: write + flush | Extend existing driver (currently read-only)                                      |
| B2   | Hypervisor: virtio-blk backend   | File-backed block device, FLUSH → `fcntl(F_FULLFSYNC)`                            |
| B3   | Filesystem service               | Port host prototype to bare-metal userspace service, `VirtioBlockDevice` impl     |
| B4   | Core integration                 | Core calls `Files` via IPC, `commit()` at `endOperation`, snapshot/restore → undo |

---

### 16 KiB Page Migration Specification

**Goal:** Change kernel page granule from 4 KiB to 16 KiB to match Apple Silicon hardware.

**Why:** macOS/iOS use 16K. Apple memory controller optimized for 16K. 4× TLB coverage per entry. The hypervisor runs on Apple Silicon — match the host.

**Key insight: 2-level page tables instead of 4.** With 16K granule and T0SZ=28 (36-bit VA = 64 GiB), only L2+L3 are needed. Current userspace layout tops out at 4 GiB. 64 GiB is massive headroom.

**ARM64 target configuration:**

```text
16K granule, T0SZ=28, T1SZ=28:
  L2: bits [35:25] → 11 bits → 2048 entries → 16 KiB table
  L3: bits [24:14] → 11 bits → 2048 entries → 16 KiB table
  Offset: bits [13:0] → 14 bits → 16 KiB page
```

TCR_EL1 changes:

- TG0: current `00` (4K) → `10` (16K). Field is bits [15:14].
- TG1: current `10` (4K) → `01` (16K). Field is bits [31:30].
- T0SZ: current `16` → `28`. Reduces user VA from 256 TiB to 64 GiB.
- T1SZ: current `16` → `28`. Reduces kernel VA from 256 TiB to 64 GiB.

**Files to change:**

| File                | Changes                                                                                                                                                                                                                                        |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `paging.rs`         | `PAGE_SIZE = 16384`. `PA_MASK` adjust (bits [13:0] now offset). VA layout: review all constants (all within 4 GiB, fine for 64 GiB VA). `USER_STACK_PAGES` may need adjustment (fewer pages for same stack size since each page is 4× larger). |
| `boot.S`            | TCR_EL1: TG0, TG1, T0SZ, T1SZ. Boot tables: `.space 16384` (×6 or fewer — only need L2+L3 with 2-level walk). Boot mapping setup: populate 2-level tables. `.align 12` → `.align 14` for 16K alignment.                                        |
| `address_space.rs`  | Drop `l0_idx`/`l1_idx`. Keep `l2_idx`/`l3_idx` with new shifts: `l2_idx = (va >> 25) & 0x7FF`, `l3_idx = (va >> 14) & 0x7FF`. `map_inner`: 2-level walk. `free_all`: 2-level walk. Root table is L2. Loops: `0..512` → `0..2048`.              |
| `memory.rs`         | Kernel boot mapping: alignment to 16K. Section protection boundaries.                                                                                                                                                                          |
| `page_allocator.rs` | `RAM_PAGES` shrinks 4× (16384 pages → 4096 pages for 256 MiB). `MAX_ORDER` adjusts. Buddy XOR uses new PAGE_SIZE.                                                                                                                              |
| `slab.rs`           | 4× more objects per slab page. Consider adding 4096/8192 size classes.                                                                                                                                                                         |
| `link.ld`           | `ALIGN(4096)` → `ALIGN(16384)`. Stack `.space` sizes. Section alignment.                                                                                                                                                                       |
| `exception.S`       | Emergency stacks: `4096 * 8` → `16384 * 8` (one 16K page per core). Stack offset arithmetic: `lsl x4, x4, #12` → `lsl x4, x4, #14`.                                                                                                            |
| `process.rs`        | ELF segment loading: page alignment. Stack page count (fewer pages, same total size).                                                                                                                                                          |
| `thread.rs`         | Kernel thread stack allocation: adjust page count.                                                                                                                                                                                             |
| `syscall.rs`        | `MAX_WRITE_LEN` review. Page alignment masks. `memory_share` checks.                                                                                                                                                                           |
| `test/tests/*.rs`   | All PAGE_SIZE-dependent tests. TLBI instruction operands if using page-granule invalidation.                                                                                                                                                   |

**Verification:** All ~2,236 tests must pass. Boot QEMU + hypervisor. Visual test (screenshot) to confirm display pipeline still works end-to-end.

### Relationship to existing decisions

- **Decision #12 (Undo):** Undo = restore to previous snapshot. Multi-file snapshots needed for compound documents. Sequential undo walks operation log backward.
- **Decision #14 (Compound documents):** All documents are manifests. COW atomicity via sole-writer + multi-file snapshots.
- **Decision #16 (Tech foundation):** This journal entry progresses the remaining sub-decision (COW on-disk design). Layer map + tentative decisions narrow the design space.
- **Decision #7 (File organization):** Flat namespace + metadata DB aligns perfectly. The metadata DB is core's concern, built on top of the flat file store.
- **Decision #5 (File understanding):** Mimetype moves from filesystem to core's metadata DB. Still OS-managed (core manages it), just not filesystem-stored.

---

## Image Decoding as a Service Interface (2026-03-24)

**Status:** IMPLEMENTED (2026-03-25) — PNG decoder factored into sandboxed service

### The question

The PNG decoder (`libraries/drawing/png.rs`) is an in-process library call from core. To support JPEG, BMP, WebP, GIF, and future formats: do we hand-write every decoder, use no_std libraries, or design an extension point?

### Why hand-writing every decoder doesn't scale

PNG (subset: 8-bit RGB/RGBA only) is ~700 lines. A baseline JPEG decoder is ~800. These are the easy formats. WebP lossy needs a VP8 decoder (~5,000+ lines). AVIF needs AV1 (tens of thousands). TIFF is tag soup with dozens of compression modes. Hand-writing works for 2-3 formats; it collapses after that.

### Why no_std libraries are a poor fit

The Rust `image-rs` and `zune-image` ecosystems exist but assume `alloc` — they allocate and return owned buffers. Our decode contract is "write BGRA pixels into this caller-provided shared memory region." Adapting library crates to that model means either forking their buffer management (losing the library benefit) or building an allocator shim (fragile coupling). The `no_alloc` constraint is the real blocker.

### The answer: decoders are sandboxed services

ALL image decoders become out-of-process services, including PNG and JPEG. This is an evolution from the Content Pipeline entry (below) which proposed "in-process library for simple formats, out-of-process service for complex ones like video." The new position: **no in-process special cases.** Reasons:

1. **Uniform interface.** One IPC protocol for all decode operations. No conditional "is this format simple enough for in-process?" dispatch.
2. **Security.** Image parsers are historically the #1 attack surface in document-handling systems (CVE databases are full of PNG/JPEG/TIFF parser exploits). Moving them out of core means a malformed file crashes a decoder service, not the OS service. Core shows a placeholder.
3. **Scalability.** Hand-write PNG and JPEG as built-in decoder services (we own the core experience). Define the IPC protocol so third parties can contribute decoders for additional formats. The interface is the design; implementations are leaf nodes.
4. **Allocator freedom.** Each decoder service has its own heap (via `sys` library's GlobalAlloc). This means decoder implementations CAN use `alloc` — `Vec`, `String`, dynamic buffers — without contaminating core's memory model. Library crates that require `alloc` become viable inside a decoder service.

### IPC protocol sketch

**Decode request** (core → decoder, event ring):

- File Store offset + length (where the raw encoded bytes live)
- Content Region output offset + max length (where to write decoded BGRA pixels)
- Request ID (for matching response)

**Decode response** (decoder → core, event ring):

- Request ID
- Status (success / unsupported format / corrupt data / buffer too small)
- Width, height (decoded image dimensions)
- Actual byte length written to Content Region

**Header-only query** (for layout without full decode):

- Same request, flag for "header only — report dimensions, don't decode"
- Response carries width + height, zero bytes written

### Memory region access changes

Current: File Store is core-private (core-write, no other readers).
New: File Store becomes core-write, decoder-read. Decoder services get **read-only** mappings of the File Store. This is the same trust model as Content Region (core-write, render-read). Render services still never see File Store.

```text
File Store ────→ Decoder service (read-only) ────→ Content Region
  (raw bytes)    (sandboxed, per-format)            (decoded BGRA)
                       ↑                                  ↓
                  IPC request                       IPC response
                  from core                          to core
                       ↑                                  ↓
                     Core ─────────────────────────→ Scene Graph
                     (layout, content semantics)     (content_id ref)
```

### Performance: not a concern

The IPC boundary carries only control messages (~64 bytes). The bulk data (raw file bytes, decoded pixels) lives in shared memory regions that the decoder reads/writes directly. Zero copies across the IPC boundary. The IPC round-trip adds ~5-10 microseconds of scheduling overhead; a JPEG decode of a 1080p image takes 5-50 milliseconds of compute. The overhead is <0.1%.

This is the same pattern that makes the scene graph IPC viable: shared memory for data, event rings for coordination.

### Migration path

1. ~~**Define the decode protocol**~~ **Done (2026-03-25).** `protocol/decode.rs` — `DecodeRequest`, `DecodeResponse`, `DecoderConfig`, `DecodeStatus`, header-only flag. Format-agnostic: same types for all decoders.
2. ~~**Factor PNG decoder into a service.**~~ **Done (2026-03-25).** `services/decoders/png/main.rs` — sandboxed process that calls `drawing::png`. Gets File Store (RO) + Content Region (RW) from init. Core sends requests via IPC, decoder writes pixels directly to shared memory. Zero copies across IPC boundary.
3. ~~**Update core**~~ **Done (2026-03-25).** Core sends header-only query (get dimensions), allocates Content Region space, sends full decode request, writes registry entry from response. Loop-wait pattern handles spurious wakeups. `drawing::png` dependency still linked (used by host tests via drawing library).
4. **Build JPEG decoder** as a second decode service (`services/decoders/jpeg/`), validating the protocol works for a second format.
5. ~~**Update init**~~ **Done (2026-03-25).** Init spawns `png-decode`, shares File Store (RO) + Content Region (RW), creates core↔decoder channel (handle 4 in core, handle 1 in decoder). Decoder starts before core (core sends request at boot).

### Implementation notes (2026-03-25)

- **Channel handle layout in core:** 0=init, 1=input, 2=compositor, 3=editor, 4=decoder, 5=tablet. Decoder inserted before variable-count input devices.
- **ImageConfig changed:** `file_store_offset`/`file_store_length` (u32) replaced `image_va`/`image_len` (u64). Core no longer needs File Store VA.
- **Spurious wakeup handling:** `sys::wait` can return from stale signals. Core uses a loop-wait pattern: `loop { wait; if try_recv → break }`. Same pattern used in all long-lived services.
- **Service location:** `services/decoders/` (not `services/drivers/`) — decoders are content services, not hardware abstractions. JPEG, WebP, etc. will be siblings of `png/`.

### Deferred

- Decoder service discovery / registration (how does core know which decoder handles which mimetype? Static table in init? Dynamic registration?)
- Multiplexed vs per-format services (one service per mimetype, or one service that loads format plugins?)
- Streaming decode for progressive JPEG / interlaced PNG (partial results before full file is decoded)
- Decode priority (visible images first, offscreen deferred)
- Whether the `drawing::png` library module should be deleted or kept for host-side tests

### Relationship to existing design

- **Content Pipeline Architecture (below):** This entry evolves the "in-process library for simple formats" to "all decoders are services." The three memory regions, scene graph content types, and Content Region design are unchanged.
- **Decision #5 (file understanding):** Mimetype is the dispatch key. Core looks up mimetype → decoder service mapping. No format sniffing in the decoder protocol.
- **Decision #13 (compound documents):** Translators that convert external formats to native compound docs are a superset of image decoders. The decoder protocol could be the foundation for the translator service interface.
- **Architecture principle:** "Does it require understanding what content IS? → OS service." Decoders understand content formats. They are services, not library utilities baked into core.

---

## Float16 Rendering Pipeline + Ordered Dithering (2026-03-24)

**Status: IMPLEMENTED**

### The problem

Document drop shadows over a #202020 desk background showed visible banding — discrete concentric bands instead of a smooth gradient. The analytical Gaussian shader (`erf` integrals) produces float-precision alpha, but the 8-bit render target quantizes to 256 levels.

### Why per-shader dithering is wrong

The naive fix is to add dither in each shader that produces gradients (shadow, rounded-rect, etc.). This fails because:

1. **Dither must be at the quantization boundary.** The quantization happens after compositing + sRGB encoding, not in individual shader outputs. Dithering alpha pre-compositing requires compensating for two nonlinear transforms (blending + sRGB gamma), making the amplitude background-dependent.
2. **It doesn't compose.** Every new visual effect that produces smooth gradients needs its own dither hack with its own amplitude correction.

### The fix: float16 intermediate

Following production renderer practice (Filament, Unreal), the entire rendering pipeline now operates in RGBA16Float:

1. **TEX_MSAA** changed from `bgra8Unorm_srgb` to `rgba16Float` (4x MSAA).
2. **TEX_RESOLVE** (new): MSAA resolves to this float16 non-MSAA texture.
3. **`fragment_dither`** (new): single fullscreen pass reads TEX_RESOLVE, applies 4x4 Bayer ordered dither in sRGB space (+/-0.5 LSB — the standard amplitude), outputs to the 8-bit sRGB drawable.

All MSAA render pipelines now specify `PIXEL_FORMAT_RGBA16F` via a new `pixel_format` field in the `create_render_pipeline` protocol command (previously hardcoded to `bgra8Unorm_srgb` in the hypervisor). The dither pipeline and blur overlay use `PIXEL_FORMAT_BGRA8_SRGB` for the drawable.

### Why this is correct

- **Dither at the quantization boundary.** The Bayer threshold is added in sRGB space, exactly where 8-bit quantization occurs. Standard +/-0.5/255 amplitude works without any background-dependent correction.
- **Composes universally.** Every visual effect (shadows, rounded rects, transparency, future gradients) gets correct dithering from the single pass.
- **Higher blending precision.** Alpha compositing accumulates in float16 instead of 8-bit, eliminating per-layer rounding error.
- **4x4 Bayer matrix** via bit-interleave: `((x0^y0)<<3 | y0<<2 | (x1^y1)<<1 | y1)`. Optimal threshold matrix — minimizes max spatial frequency of quantization error.

### Protocol change

`create_render_pipeline` payload: 16 bytes -> 17 bytes. New `pixel_format: u8` field at offset 16. Mandatory — hypervisor rejects undersized payloads with a diagnostic message. No backward-compatibility fallback (kill the old way).

Verified numerically: longest band 34px -> 12px, average band 7.4px -> 1.9px.

---

## Scene Tree Z-Order: Content Under Title Bar (2026-03-24)

**Status: IMPLEMENTED**

### The problem

Document drop shadows extend beyond the document bounds (blur + spread). The content viewport (`N_CONTENT`) had `CLIPS_CHILDREN` and started below the title bar (`y = title_bar_h`). Shadows extending upward were hard-clipped at the content boundary, creating a visible cutoff.

### The fix

Restructured the scene tree sibling chain from:

```text
N_ROOT → N_TITLE_BAR → N_SHADOW → N_CONTENT → N_POINTER
         (low z)                    (high z)
```

to:

```text
N_ROOT → N_CONTENT → N_TITLE_BAR → N_POINTER
         (low z)     (high z)
```

Changes:

- `N_CONTENT`: y=0, height=fb_height (full screen, was clipped below title bar)
- `N_STRIP`: y=content_y (offset to position documents below title bar)
- `N_TITLE_BAR`: paints AFTER content (higher z-order), overlays shadows
- `N_SHADOW`: unused (was a zero-height placeholder for a chrome shadow gradient)

The title bar is transparent (`CHROME_BG = TRANSPARENT`), so shadows show through it while title text/icon/clock render on top. The `CLIPS_CHILDREN` on `N_CONTENT` still prevents horizontal document leakage during slide transitions.

---

## Scheduling budget starvation — 120 Hz animation ran at ~20 Hz (2026-03-24)

**Status: FIXED**

### Symptom

Slide animation (Ctrl+Tab) appeared to run at ~5-10 FPS instead of 120 FPS. Core's main loop requests `sys::wait(handles, 8_333_333)` (8.3ms for 120 Hz). Actual wait times alternated between **~8.9ms** (correct) and **~41ms** (5× late), giving ~20 Hz effective update rate.

### Root cause

All user threads shared a single default scheduling context: **10ms budget per 50ms period** (20% of one core). The core service consumed ~8.3ms per frame — nearly the entire 10ms shared budget. After one animation frame, the budget was exhausted. The kernel's `reprogram_next_deadline` programmed CNTV_TVAL for the scheduler's replenishment deadline (~41ms remaining in the 50ms period) instead of the sys::wait timeout (8.3ms). The core thread waited ~41ms for replenishment before running again.

Kernel instrumentation confirmed: TVAL values on "slow" frames were ~1,000,000 ticks (41ms at 24 MHz) — matching the scheduler replenishment period, not the wait timeout.

### Fix

`kernel/scheduler.rs`: Changed `DEFAULT_BUDGET_NS` from 10ms to 50ms (= period). With budget=period, the budget is effectively unlimited. EEVDF still provides fairness via virtual time. Per-service budgets can be implemented later via the existing `scheduling_context_create`/`scheduling_context_bind` syscalls.

After fix: consistent **8-9ms per frame** across all 64 frames of the animation. No alternating pattern.

### Also fixed: hypervisor IMASK re-arm detection

`~/Sites/hypervisor/Sources/VCPU.swift`: Changed vtimer re-arm detection from ISTATUS-based to CNTV_CVAL-based. The old check (`ISTATUS==0`) missed re-arms where the new timer fired before the hypervisor checked — a fast timer sets ISTATUS=1, making the hypervisor think the guest hadn't re-armed. The new check compares CNTV_CVAL against the value captured at mask time; any change means the guest wrote a new deadline.

### Also fixed: core animation improvements

`services/core/main.rs`:

1. **First-frame dt clamp** (`slide_first_frame` flag): Spring ticks with nominal frame interval on the animation-start frame instead of accumulated idle time (up to 50ms). Prevents ~35% first-frame jump.
2. **Slide-only dispatch path**: Slide animation no longer sets `changed=true`, avoiding unnecessary `update_cursor` dispatch on every animation frame. Uses `slide_changed` in `needs_scene_update` for its own dedicated publish path (1 scene buffer copy instead of 3).

### Future: per-service scheduling budgets

Current fix (budget=period) is effectively unlimited — fine with 4 cores and ~5 threads. When contention becomes real, the architecture naturally supports **static allocation with dynamic binding**:

- Init creates named scheduling contexts at boot (e.g., "idle" 1ms/500ms, "animation" 8.5ms/8.3ms, "render" 8.5ms/8.3ms).
- Services use `scheduling_context_borrow`/`return` (syscalls already exist) to switch between contexts based on current workload. Core borrows "animation" on Ctrl+Tab, returns on settle.
- Budgets are static (init decides at spawn). Binding is dynamic (services switch at runtime). No negotiation protocol needed.

**Open questions for that future:** shared vs separate budgets for core+render during animation; borrow reference counting when multiple animations overlap; defensive timeout if a borrow is never returned; whether the complexity is ever justified given EEVDF fairness without budgets.

---

## Content Pipeline Architecture (2026-03-24)

**Status:** IMPLEMENTED (2026-03-24). Content Region + File Store + PNG decode + registry-based font lookup.

### The question

How should image content (and eventually video, audio, compound parts) flow from file bytes to pixels on screen? Triggered by a concrete question: switching the test gradient image to the real test.png loaded via 9p.

### The wrong answer we almost built

The initial proposal (Option B) was: core references encoded PNG bytes in the scene graph, the render service decodes and caches. This seemed efficient (render service owns textures, no pixel copy through the scene graph) and followed the font precedent (render services parse TTF bytes directly).

**This violates the architecture.** The decision checklist (`architecture.md:191`) is unambiguous:

- "Does it require understanding what content _is_? → OS service."
- "Does it turn positioned visual elements into pixels? → Compositor."
- "If you find yourself adding content-type awareness to the compositor, the responsibility is in the wrong place."

PNG decoding is content-type understanding, not visual rendering. The decoded BGRA pixels are a visual primitive; the PNG file is a content artifact. The compositor must never see encoded files.

**The font precedent was misleading.** Glyph rasterization in the compositor IS visual primitive rendering (device-dependent: hinting, density, AA). Image decoding is NOT (device-independent: same pixels regardless of display). The architecture document explicitly places glyph rasterization in the compositor (line 74, 87). There is no anomaly to resolve.

### The three data transformations

The content pipeline has exactly three stages. Each is a data transformation with a clear interface:

```text
File bytes  ──[decode]──→  Decoded content  ──[layout]──→  Scene graph  ──[render]──→  Pixels
              (leaf node)                     (OS service)                (compositor)
```

| Stage  | Responsibility                                                                   | Knows about     |
| ------ | -------------------------------------------------------------------------------- | --------------- |
| Decode | Format internals (PNG chunks, JPEG DCT, MP4 atoms)                               | One format      |
| Layout | Content semantics (text wraps, images have dimensions, parts have relationships) | Content types   |
| Render | Visual primitives (rectangles, pixel blits, glyph outlines, Bézier paths)        | Geometry, color |

Decoders are leaf nodes — complex inside, simple interface. The interface is stable; implementations are swappable. **Updated 2026-03-24:** All decoders are now out-of-process services, including simple formats like PNG — see "Image Decoding as a Service Interface" above.

### Three memory regions

| Region             | Writer              | Reader            | Contents                                                                                 | Lifetime                   |
| ------------------ | ------------------- | ----------------- | ---------------------------------------------------------------------------------------- | -------------------------- |
| **File Store**     | Filesystem (9p/blk) | Core only         | Raw encoded file bytes (PNG, TTF, MP4)                                                   | Tied to file on disk       |
| **Content Region** | Core (via decoders) | Core + Compositor | Decoded pixels, font TTF data, frame rings                                               | Tied to open documents     |
| **Scene Graph**    | Core                | Compositor        | Visual primitives + small inline data (glyph arrays, path commands, small pixel buffers) | Per-frame, triple-buffered |

**The compositor never sees the File Store.** It only reads decoded/rendering data from the Content Region and visual primitives from the Scene Graph.

Today these are conflated: the "font buffer" carries raw fonts AND raw PNG bytes AND is shared with everyone. The font buffer needs to become the Content Region (decoded rendering data, shared with compositor) with raw file bytes staying in core-private memory (File Store).

Note: font TTF data belongs in the Content Region, not the File Store, because it's rendering data — the compositor reads it for glyph rasterization. The fact that it happens to be the same bytes as the file on disk is incidental.

### Content Region design

The Content Region is a persistent shared memory region with a registry:

- **Allocated by init** at boot (generous fixed size, e.g., 32–64 MB)
- **Managed by core** — core decides what's loaded, allocated, evicted
- **Read-only for compositor** — compositor reads at offsets the registry specifies

Registry: fixed header with entry table. Each entry: offset, length, content class (font/pixels/frame-ring), generation counter (for cache invalidation in the compositor).

**Write-once semantics for concurrency.** Entries are immutable once written. Content updates (image re-decoded after edit) create new entries with new content_ids. The scene graph generation is the synchronization mechanism — the compositor reads the old entry until it picks up the new scene graph generation referencing the new entry. Old entries are freed via generation-based GC: with triple buffering, at most 3 generations are in flight; core frees entries no longer referenced by any active generation.

**The Content Region is a decode cache.** File Store is the source of truth. Content Region holds the hot working set. Eviction policy (e.g., LRU for offscreen documents) is core's concern. Re-decode from File Store when scrolled back. Needs a real allocator (free-list with coalescing, not just bump).

### Scene graph content types

```text
None            — pure container (rectangles, selection highlights)
InlineImage     — small pixel buffer in inline data buffer (icons, cursors)
Image           — decoded pixels in Content Region (photos, illustrations)
Stream          — time-varying frame ring in Content Region (video, animation)
Path            — cubic Bézier vector geometry
Glyphs          — shaped glyph IDs (compositor rasterizes from font data in Content Region)
```

`InlineImage` is for small, per-frame, potentially regenerated content. `Image` is for large, persistent, decoded content. The distinction is explicit in the type. (Long-term, InlineImage may merge into Image if all persistent pixel data moves to the Content Region.)

### How each content class flows through

**Text:** UTF-8 bytes → (no decode needed) → core shapes + lays out → Glyphs nodes in scene graph → compositor rasterizes from font data in Content Region.

**Image:** PNG bytes in File Store → core calls png_decode() library → BGRA pixels in Content Region → core reads header for dimensions, lays out → Image node in scene graph (content_id, w, h) → compositor blits from Content Region. Compositor never sees PNG.

**Video (future, v0.6):** MP4 in File Store → decode service (separate process) demuxes/decompresses → frame ring in Content Region → core sets playhead (start time, play/pause/seek state) → Stream node in scene graph → compositor samples current frame by wall-clock time. Core does not run at video framerate; it sets the playhead once and the compositor computes which frame to show at render time (same pattern as cursor blink — time-dependent rendering is a compositor concern).

### Format discrimination

The scene graph `Image` node carries a `content_id` referencing a Content Region entry. It does not carry a mimetype or format hint. The compositor doesn't need to know the source format — it sees decoded BGRA pixels. All format discrimination happens before the scene graph, in core's decoder dispatch (keyed on the document's mimetype, which is OS-managed metadata per Decision #5).

### Dimensions from file headers

Core needs image dimensions for layout without full decoding. Every image format puts dimensions in a fixed-position header (PNG: bytes 16-23; JPEG: SOF marker scan; WebP: bytes 12-29; GIF: bytes 6-9; BMP: bytes 18-25). A header-only parse function per format (~30 lines each) is sufficient. JPEG is the only one requiring a marker scan; all others are at fixed offsets.

### What this means for the current code

1. Rename/redesign "font buffer" → Content Region (with registry, allocator, generation tracking)
2. Split file loading: raw bytes stay in core-private memory (File Store), decoded content goes to Content Region
3. Move PNG decoder from `test/tests/png_decoder.rs` to a library callable by core
4. Core decodes PNG into Content Region, writes Image node with content_id
5. Delete `generate_test_image()` in core
6. `CompositorConfig` carries Content Region base VA (replaces `font_buf_va`)
7. Render services resolve content_ids via Content Region registry

### Deferred

- ~~Content Region allocator implementation (free-list with coalescing)~~ **Done (2026-03-25).** `ContentAllocator` in `protocol/content.rs`. Free-list with first-fit, sorted-by-offset, automatic coalescing. 16-byte alignment. Init bump-allocates fonts; core initializes free-list from remaining space. `remove_entry()` companion for registry removal. 27 tests.
- ~~Generation-based GC for stale entries~~ **Done (2026-03-25).** Deferred reclamation via `defer_free()` + `sweep()` on `ContentAllocator`. Core retires content_id at death_gen; sweep frees when `reader_done_gen >= death_gen`. Leverages triple-buffer generation counter (already existed). `TripleWriter::reader_done_gen()` exposed. `SceneState` generation accessors. Entry creation stamped with current scene generation. Sweep runs in event loop after scene publish (only when pending > 0). 9 additional tests. Same pattern as RCU — grace period is the triple-buffer generation gap.
- Stream content type and frame ring layout (v0.6)
- Whether InlineImage can be eliminated entirely (all persistent pixels in Content Region)
- Content Region growth/resize strategy (if fixed size proves insufficient)

---

## Coordinate Model: Fixed-Point Units (2026-03-23)

**Status:** SETTLED (2026-03-23). Unit: 1/1024 pt. Absolute scroll. AffineTransform stays f32.

### The decision

**Internal coordinate unit: 1/1024 pt (10-bit fractional point).** All scene graph positions, dimensions, and offsets use `i32` (signed) or `u32` (unsigned) in this unit. Precision: 0.001 pt (sub-pixel at any density). Range: ±2,097,151 pt (±2,489 A4 pages). Conversion: bit shift `>> 10`.

### What motivated it

Two f32 bugs in one session:

1. **Spring instability:** Semi-implicit Euler diverged at dt > 33ms because f32 arithmetic amplified the overshoot.
2. **Settle precision:** At value ≈ 2056, f32 can't represent positions closer than ±0.00024. The spring got stuck 0.001 away from its target.

Neither bug would exist with integer arithmetic. Every major GUI system converges on the same pattern: integers or fixed-point for positions/layout, float only at the compositor transform and GPU boundaries.

### Resolved questions

**Width/height type:** `u16` → `u32` (1/1024 pt). The `_reserved` field shrinks from `[u8; 8]` to `[u8; 4]` — Node stays at exactly 136 bytes.

**1/1024 vs 1/64 (FreeType 26.6):** 1/1024. The "match FreeType" argument doesn't apply — this system has its own rasterizer with 16.16 glyph advances. The extra precision (16× over 1/64) costs nothing (same i32 storage). 1/64's extra range (39K vs 2.5K pages) is wasted on a personal workstation.

**Scroll range:** Absolute offset, no viewport-relative scheme. i32 at 1/1024 pt covers 2,489 A4 pages. A 1,000-page document uses 40% of the range. If a future content type needs infinite scroll, the content type's layout handler implements virtual scrolling internally (leaf-node complexity behind a simple interface).

**AffineTransform:** Stays f32. Transform coefficients (a, b, c, d) are dimensionless multipliers — not "in points." Quantizing rotation coefficients to 1/1024 produces visible error on large elements (cos(30°) error × 1000 pt ≈ 0.19 pt). Transforms compose via matrix multiplication (different from position accumulation) and go straight to the GPU (which mandates f32). Every major compositor keeps transforms in float.

### Integer/float boundary

| Component                   | Type            | Unit               | Rationale                                   |
| --------------------------- | --------------- | ------------------ | ------------------------------------------- |
| Node.x, Node.y              | `i32`           | 1/1024 pt          | Positions accumulate, need exact comparison |
| Node.width, height          | `u32`           | 1/1024 pt          | Dimensions need same unit as positions      |
| scroll_offset, slide_offset | `i32`           | 1/1024 pt          | The actual bug sites                        |
| Spring internals            | `f32`           | unitless           | Math is natural in float (sin, exp)         |
| Spring::value() output      | → `i32`         | 1/1024 pt          | Convert at API boundary                     |
| AffineTransform             | `f32`           | dimensionless + pt | Render boundary, GPU-bound                  |
| Render services             | convert i32→f32 | pixels             | Already happens for node.x/y                |
| Glyph advances (internal)   | 16.16 fixed     | 1/65536 pt         | Truncate to 1/1024 at layout boundary       |

### Prior art

| Layer                 | What they use                                         | Why                                |
| --------------------- | ----------------------------------------------------- | ---------------------------------- |
| Document layout       | Integers (TeX scaled points, LibreOffice twips)       | Determinism, no drift              |
| Protocol/wire format  | Fixed-point (Wayland 24.8) or integers (X11, Android) | No NaN/Inf, compact, deterministic |
| Font metrics          | Fixed-point (FreeType 26.6)                           | Matches TrueType spec              |
| 2D rasterizer         | Fixed-point (Cairo 24.8, Direct3D 16.8)               | Exact scanline intersections       |
| Compositor transforms | Float (macOS CGFloat, DWM)                            | Rotation/scale produce irrationals |
| GPU vertex shader     | f32 (hardware mandated)                               | No choice                          |

---

## Animation Tick Architecture (2026-03-23)

**Status:** SETTLED and IMPLEMENTED (2026-03-23).

### The solution

Unified animation timeout: single `any_animation_active()` check (scroll spring || slide spring || timeline.any_active()). If anything is animating, wake at the actual display refresh rate (from hypervisor via render service → init → core). Otherwise sleep until the next blink phase transition.

Refresh rate plumbing: hypervisor exposes `NSScreen.maximumFramesPerSecond` at virtio config offset 0x08. Metal-render reads it and reports in `DisplayInfoMsg`. Init distributes to core via `MSG_FRAME_RATE` message (separate from `CoreConfig` which is full at 56 bytes due to u64 alignment).

On ProMotion displays, animations now tick at 120 Hz instead of hardcoded 60 Hz. QEMU path defaults to 60 Hz (no refresh rate exposed).

---

## IPC: State Registers vs Event Rings (2026-03-23)

**Status:** Design decided. Implementation pending.

### The problem

The IPC channel between virtio-input and core overflows under sustained input. The ring holds 62 messages. When core is busy (scene rebuild, text shaping), pointer movement events pile up and are silently dropped: `virtio-input: ring full, event dropped`.

### The insight

Input data has two fundamentally different semantics:

- **Events** (discrete, every one matters): key press, key release, mouse button click. Order and count are significant. A ring buffer is the correct abstraction.
- **State** (continuous, latest wins): pointer position, tablet pressure, modifier bitfield. Intermediate values between frames are waste. Only the latest matters. A ring buffer is the _wrong_ abstraction — it forces the consumer to drain N messages when it only needs the last one, and overflows when N exceeds capacity.

Every major OS converges on this distinction: macOS coalesces mouse moves in WindowServer. Windows explicitly coalesces WM_MOUSEMOVE between GetMessage() calls. Linux/libinput merges motion events at frame boundaries. Plan 9's /dev/mouse is a current-state file, not an event stream.

### The decision

**Separate the two concerns at the IPC level:**

1. **Event ring** (existing): SPSC ring buffer for discrete events. Keep as-is. Consider adding a dropped-event signal (like Linux SYN_DROPPED) so the consumer can resync modifier state.
2. **Shared state register** (new): init-allocated shared memory for continuous state. The driver atomically overwrites the latest value. Core reads it once per frame. Zero queue, zero overflow, always current.

**Ownership:** Init allocates the shared memory region and maps it into both processes. Same pattern as the scene graph (core → render) and font data (init → core). No changes to the IPC library or kernel.

**Notification:** The existing `channel_signal` on the IPC channel. The signal means "something changed" — core wakes and checks both the event ring and the state register. Still event-driven, not polling.

### Why not extend the IPC channel?

Making "event ring + state register" a first-class IPC primitive was considered. It's more general but changes a foundational interface. The init-as-orchestrator pattern already handles shared state between processes (scene graph, fonts). Promoting to a first-class IPC feature is a future option if the pattern recurs — for now, init-allocated is simpler and equally correct.

### Prior art in this system

| Shared memory region           | Writer           | Reader         | Pattern                 |
| ------------------------------ | ---------------- | -------------- | ----------------------- |
| Scene graph                    | core             | render service | State (triple-buffered) |
| Font data                      | init (loader)    | core           | Read-only               |
| IPC channel ring               | virtio-input     | core           | Events                  |
| **Input state register** (new) | **virtio-input** | **core**       | **State (single slot)** |

### Implementation plan

1. Define `InputState` struct in protocol (pointer x/y, button bitfield, generation counter)
2. Init allocates a shared page, maps into virtio-input and core
3. Init sends the address to both via config messages
4. virtio-input writes `InputState` on pointer events (atomic store + channel_signal)
5. Core reads `InputState` once per frame (atomic load), drains event ring for keys/buttons
6. Remove MSG_POINTER_ABS from the event ring

---

## Icon Implementation (2026-03-23)

**Status:** Complete. Pipeline working, icons rendering cleanly.

### What was built

Full icon rendering pipeline: SVG path parser (`scene/svg_path.rs`), stroke expansion engine (`scene/stroke.rs`), arc-to-cubic conversion, `stroke_width` field on `Content::Path`, two-sided Metal stencil, CPU pre-rasterization in core, `Content::Image` display in title bar. Tabler file-text icon renders at correct size in the title bar. Pointer cursor redesigned to Tabler proportions.

### Spike artifacts (resolved)

Stroke-expanded geometry had spike artifacts at corners. The spikes persisted even with round join arcs disabled (bevel-only), indicating the issue was in the structural offset contour logic, not the arc math. Fixed by the user between sessions.

---

## Decision #18: Iconography (2026-03-22)

**Status:** Settled. Recorded in `decisions.md` as Decision #18.

### The question

How should the OS store, render, and map icons? Three sub-questions: (1) what format for icon data, (2) how to render them, (3) how to associate icons with mimetypes.

### What we explored

**Three format options evaluated:**

- **A: Icon font** (what macOS SF Symbols and Windows Segoe Fluent do). Pack icons as glyphs in an OpenType font, render through the existing glyph cache. Rejected: stroke-to-fill conversion needed (Tabler icons are stroke-based), philosophical mismatch (icons are content, not text), multi-color requires COLR/CPAL table complexity.
- **B: Native `Content::Path` data** (what the pointer cursor already uses). Convert SVGs to the OS's binary path commands at build time. Render through the existing path rasterizer. **Chosen.**
- **C: Custom binary icon format.** Rejected as over-engineering — the existing path command format already _is_ a compact binary format.

**Runtime vs build-time stroke rendering:** Runtime chosen. Adding stroke support to the path pipeline is a general-purpose investment (line charts, diagrams, drawing tools), not icon-specific. The same path data can render as outline or filled depending on context.

**Icon font vs path baseline alignment:** The concern was that font glyphs get automatic baseline alignment "for free." Analysis showed that aligning `Content::Path` icons with adjacent text requires ~5 lines of positioning math using font metrics already available in core (ascent, line height). Not a meaningful cost difference.

**Cursors:** Operational cursors (mouse pointer, text caret) remain hand-built geometric primitives. Tabler's cursor icons are for symbolic/UI representation, not operational use. Different requirements: operational cursors need pixel-precise hotspots at small sizes; symbolic icons need visual consistency in UI chrome.

### Source set

**Tabler Icons** (MIT license, 5,021 outline + 1,053 filled). SVG analysis across all 5,021 outline icons:

- 83% use SVG arcs (`a` command) — requires arc-to-cubic conversion
- Commands used: M (20K), a (19K), l (13K), h/v (22K), c (5K), s (487), q/t (74)
- Average ~580 bytes per SVG; estimated ~200 bytes as compiled path commands
- Outline style chosen over filled (lighter, more icons available, fill available as rendering mode)

### Implementation plan

1. Add arc, h/v, s, q command support to path pipeline (scene library)
2. Add runtime stroke rendering to path pipeline (render backends)
3. Build host-side SVG→path converter tool (`system/tools/svg2path/` or in `build.rs`)
4. Create `libraries/icons/` with compiled-in icon data and mimetype lookup
5. Integrate into core: document type icon in title bar, baseline-aligned with text

### Deferred

- Full icon set curation (starting with ~20 for known mimetypes)
- Cap height extraction from OS/2 font table (ascent works well enough)
- Hierarchical/multi-color icon rendering (architecture supports it; monochrome first)

---

## Phase 3.2: Text Editor Key Combinations (2026-03-22)

**Status:** Complete.

### Architecture decision: navigation lives in core

The spec originally placed all navigation and editing in the editor process. During implementation, the architecture was revised: **navigation and selection live in core** (the OS service), not the editor. This follows Decision #8 — the OS owns layout and provides content-type interaction primitives (cursor, selection, playhead). Editors are content-type-specific input-to-write translators.

The text editor process was reduced from ~410 to ~195 lines. It now handles only: character insert, backspace, forward delete, Tab (4 spaces), Shift+Tab (dedent). No navigation, no selection, no shift tracking.

### What was implemented

**Protocol changes:**

- `KeyEvent` gained `modifiers: u8` field with `MOD_SHIFT` (0x01), `MOD_CTRL` (0x02), `MOD_ALT` (0x04), `MOD_SUPER` (0x08), `MOD_CAPS_LOCK` (0x10) flags.

**Input driver (`virtio-input`):**

- Modifier state tracking (Shift, Ctrl, Alt, Super press/release)
- Caps Lock: set/clear matching macOS flag state (not toggle-on-press)
- Shifted ASCII: full US keyboard layout (`!@#$%^&*()` etc.)
- All key events include modifier bits

**Core (`services/core/main.rs`) — major rewrite of `process_key_event`:**

- All arrow navigation (Left/Right/Up/Down with sticky `goal_col`)
- Cmd+Left/Right (visual line start/end), Cmd+Up/Down (document start/end)
- Opt+Left/Right (word boundary navigation)
- Home/End, PgUp/PgDn
- Shift+any navigation (selection extension via `update_selection`)
- Cmd+A (select all)
- Selection-aware backspace/delete (core handles directly via `doc_delete_range`)
- Opt+Backspace/Delete (word delete, core handles directly)
- Double-click (word select), triple-click (line select)
- `forward_key_to_editor` includes `channel_signal` to wake editor

**Layout library (`libraries/layout/lib.rs`):**

- `line_col_to_byte()` — inverse of `byte_to_line_col`
- `word_boundary_backward()` / `word_boundary_forward()` — scan for whitespace transitions
- `ParagraphLayout::line_col_to_byte()` method

**Hypervisor (`~/Sites/hypervisor/`):**

- Added `.capsLock` to `handleFlagsChanged` modifier list in `AppWindow.swift`

### Bug fixes

- `forward_key_to_editor` was missing `sys::channel_signal(EDITOR_HANDLE)` — editor process never woke up for backspace/delete events
- Hypervisor didn't forward Caps Lock events (missing from `handleFlagsChanged`)
- Guest Caps Lock handling changed from toggle-on-press to set/clear matching macOS flag state

### Tests

13 new tests in `test/tests/layout.rs`: 6 for `line_col_to_byte` (basic, empty, wrapped, mid-char, beyond-end, multi-paragraph), 7 for word boundaries (backward/forward basic, at-boundary, start/end of string, multiple-spaces, non-alpha). 2,091 total tests pass.

---

## Analytical Gaussian Shadows + sRGB Pipeline (2026-03-21)

**Status:** Complete.

### Shadows

metal-render had been emitting shadows as a single solid-color quad — invisible on dark backgrounds. Replaced with an analytical Gaussian fragment shader (`fragment_shadow`) that evaluates the exact closed-form integral per-pixel:

- **Rectangles (corner_radius=0):** Separable product of two 1D erf integrals — mathematically exact. `shadow_1d(p, lo, hi, σ) = 0.5 * (erf((hi-p)/σ√2) - erf((lo-p)/σ√2))`, then `alpha = Ix * Iy`.
- **Rounded rects (corner_radius>0):** SDF distance + `erfc(d/σ√2)/2` — excellent approximation for convex shapes.
- **erf approximation:** Abramowitz & Stegun 7.1.26, max error ≤ 1.5×10⁻⁷.
- **No offscreen textures or compute passes.** The quad extends 3σ beyond the shadow rect; the fragment shader computes per-pixel analytically.

Shadow parameters on title bar: offset=2pt, blur_radius=12 (σ=6), alpha=120. SHADOW_DEPTH eliminated (content starts at y=title_bar_h, no gap).

### sRGB Render Target

Switched the MSAA texture and CAMetalLayer to `bgra8Unorm_srgb`. The Metal hardware blender now automatically converts sRGB↔linear at the framebuffer boundary, making all alpha compositing gamma-correct: rounded-rect AA edges, text over background, shadow compositing — everything.

- All fragment shaders linearize their sRGB color inputs via the existing `srgb_to_linear()` MSL function.
- Backdrop blur pipeline unaffected: compute `read()`/`write()` bypass sRGB conversion, so the manual sRGB↔linear kernels remain correct.
- Clear color changed from sRGB (0.13) to linear (0.005) to match BG_BASE.
- New protocol constant: `PIXEL_FORMAT_BGRA8_SRGB = 6`.

### Rounded-rect alpha bug

The `fragment_rounded_rect` shader was outputting premultiplied RGB (`fill.rgb * fill.a`) but the blend mode expected non-premultiplied (`srcAlpha * src + (1-srcAlpha) * dst`). This squared the alpha: the title bar rendered at RGB 27 instead of the correct 40. Fixed by outputting non-premultiplied color via weighted-average compositing for the border/fill regions.

### Rendering audit notes

- **Virgil-render has no shadow rendering** — shadow properties are silently ignored. Not blocking (metal-render is the primary path going forward).
- **LCD subpixel text rendering** intentionally skipped across all backends (grayscale coverage only).
- CpuBackend shadow rendering uses discrete 3-pass box blur (correct but different algorithm from analytical Gaussian).

---

## Rendering Test Document (2026-03-21)

**Status:** Idea — noted for future implementation.

A **visual test mode** accessible via key combo that switches to a purpose-built "rendering sample compound document." This document exercises every rendering capability systematically:

- One node per content type (None, Glyphs, Image, Path)
- One node per composition feature (clip path, backdrop blur, opacity, shadow, border)
- One node per transform type (translate, rotate, scale, skew, combined + rounded corners)
- One animated section (spring, easing curves, color lerp)
- Labeled, grid-laid-out, easy to scan at a glance

Replaces the scattered demo nodes that accumulated during v0.3. Invoked when needed for verification after rendering pipeline changes — doesn't clutter the normal editor scene.

---

## Points and Pixels: Coordinate System Terminology (2026-03-19)

**Status:** Settled.

### The question

The OS uses resolution-independent coordinates internally (scene graph, core layout, font sizing) and physical framebuffer coordinates at the rendering edge. What should we call these two units?

### Decision: Points and Pixels

**Point (pt)** = 1/72 inch. The OS's internal resolution-independent unit. Used everywhere above the render boundary: scene graph node positions and sizes, font sizes, layout constants, shadow offsets. One coordinate system for both spatial layout and typography.

**Pixel (px)** = one physical display element. Used only by the render backends and drawing library — the final stage of the pipeline where points are converted to hardware coordinates.

**Scale factor** = `physical_dpi / 72`. Derived from hardware (EDID) or user preference. The render library applies it during the scene tree walk via `scale_coord()` and `scale_size()` in `render/scene_render/coords.rs`.

### Why points, not some other unit?

- **Granularity:** At typical desktop DPIs (96–220), 1pt maps to ~1–3 physical pixels. Integer point values give near-pixel-level control without requiring fractional coordinates.
- **Typography alignment:** Points are the native unit of font metrics (advance widths, ascent/descent, kerning). Since the OS is document-centric and text is a primary content type, aligning the coordinate system with typographic conventions means font metrics can be used directly without conversion.
- **Self-documenting terminology:** "Points" and "pixels" are unambiguous. Anyone reading the code knows immediately which coordinate space they're in. "Logical pixels" invited confusion because the word "pixel" suggests a physical thing.
- **Physical meaning:** 1pt = 1/72 inch gives the unit a real-world anchor. "18pt font" means 0.25 inches tall, matching designer/typographer expectations. The scale factor becomes a derivable property of the display hardware, not an arbitrary knob.

### DPI detection

The scale factor is derived from display DPI: `scale = physical_dpi / 72`. Three sources, in priority order:

1. **User preference** — always wins. Accessibility, viewing distance, personal taste.
2. **EDID** — monitor broadcasts physical dimensions + native resolution. Compute `dpi = resolution_px / (size_mm / 25.4)`. Most desktop monitors have EDID.
3. **Default** — assume 96 DPI when detection fails (`scale = 96/72 ≈ 1.33`). Universal desktop assumption.

Detection lives in the render driver (it owns the display hardware). The OS service combines detected DPI with user preference and sends the final scale factor via `CompositorConfig`. The scene graph and core never know about DPI.

### Rejected alternatives

- **Millimeters:** Too coarse (1mm ≈ 3.78px at 96 DPI). Would require fractional coordinates (`f32`) everywhere, introducing floating-point accumulation errors in layout.
- **CSS px (1/96 inch):** Finer than points and 1:1 at 96 DPI, but an arbitrary web-era convention. Calling them "pixels" reintroduces the naming confusion.
- **Abstract logical pixels (no physical anchor):** Simpler (no DPI detection needed), but "18pt" has no physical meaning — just relatively bigger than "14pt". Loses the self-documenting property.

### Terminology audit required

All code, comments, documentation, and variable names must consistently use "points" for resolution-independent values and "pixels" for physical framebuffer values. Key locations: scene graph node fields, protocol config structs, render library coord functions, core layout constants, architecture docs.

### Key insight

There are exactly two spatial units in this system. Points (abstract, physical meaning, used by everything above the render boundary) and pixels (concrete, hardware-dependent, used only at the rendering edge). The render library's `scale_coord()` and `scale_size()` are the single conversion boundary. No third unit exists — font sizing is points, the same unit as coordinates.

---

## Phase 5: CPU Render Service Merge (2026-03-18)

**Status:** Complete. Compositor + virtio-gpu merged into single `cpu-render/` process.

### What was done

Merged `services/compositor/` and `services/drivers/virtio-gpu/` into a single `services/drivers/cpu-render/` process — a sibling to `virgil-render`. The two-process pipeline (compositor → MSG_PRESENT → virtio-gpu) is replaced by a single process that does CPU rendering and GPU presentation in the same event loop.

**Key design decision:** cpu-render self-allocates framebuffers via `dma_alloc` instead of receiving them from init. This makes its init handshake identical to virgil-render's (MSG_DEVICE_CONFIG → MSG_DISPLAY_INFO → MSG_GPU_CONFIG → MSG_GPU_READY → MSG_COMPOSITOR_CONFIG). Init's `setup_display_pipeline()` was rewritten to mirror `setup_virgl_pipeline()`.

**Eliminated:**

- Compositor→GPU IPC channel (no MSG_PRESENT/MSG_PRESENT_DONE)
- One process boundary (one fewer context switch per frame)
- Framebuffer allocation in init (~24 dma_alloc calls moved to cpu-render)
- Scatter-gather PA table messaging (MSG_FB_PA_CHUNK)
- ~175 lines of init code

**Files:**

- Created: `cpu-render/main.rs` (457 lines), `cpu-render/gpu.rs` (524 lines), `cpu-render/frame_scheduler.rs` (173 lines, copied from compositor)
- Modified: `build.rs` (replaced compositor+virtio-gpu with cpu-render), `init/main.rs` (rewritten setup_display_pipeline)
- Deleted: `services/compositor/`, `services/drivers/virtio-gpu/` (no parallel implementations)

### Architecture (post-merge)

```text
Init probes GPU features
  ├── VIRTIO_GPU_F_VIRGL set:
  │   Core → Scene Graph → virgil-render (Gallium3D → virglrenderer → ANGLE → Metal → GPU)
  └── No virgl:
      Core → Scene Graph → cpu-render (CpuBackend + virtio-gpu 2D) → Display
```

Both render services follow the same pattern: single process, same handshake, scene graph as sole input interface. The only difference is what happens inside (Gallium3D commands vs CPU rasterization + 2D transfer).

### Testing

- 1,816 host-side tests pass
- QEMU visual verification: all content types render correctly (text, images, paths, backgrounds, clock)
- Stress test: no crashes under 4 SMP cores
- Boot serial output confirms clean handshake sequence

### Remaining deferred work

- **Remove test content** from `scene_state.rs` once real Image/Path producers exist.
- ~~**Extract common handshake** from setup_virgl_pipeline and setup_display_pipeline.~~ **Done (2026-03-18).** Unified into single `setup_render_pipeline(name, ...)` function. The `name` parameter (`b"virgl"` or `b"cpu-render"`) drives diagnostic output only. Reduced init from ~1,500 lines to ~1,076 lines.

---

## Virgl Task 8: Init Integration — Render Service Selection (2026-03-18)

**Status:** Complete. Virgl implementation plan fully executed (all 8 tasks).

---

## Virgl Task 7: Image Rendering + Path Groundwork (2026-03-18)

**Status:** Image rendering complete and visually verified. Path stencil-then-cover code written but blocked on ANGLE.

### What was done

**Content::Image — full pipeline working:**

- Core (`scene_state.rs`): generates test 32×32 BGRA gradient, pushes to data buffer, creates `Content::Image` node
- Scene walk (`scene_walk.rs`): detects `Content::Image`, records screen position + DataRef in `ImageBatch`
- Driver (`main.rs`): creates BGRA8 GPU texture (IMG_RESOURCE_ID=5), copies pixel data from scene graph data buffer → DMA → `TRANSFER_TO_HOST_3D`, renders as textured quad with `TEXTURED_FS` (full RGBA sampling)
- Visually verified: gradient image renders correctly in QEMU at 2× Retina scale

**Content::Path — code complete, stencil blocked:**

- Core: generates test star (5-pointed, LineTo only) and rounded rectangle (CubicTo curves)
- Scene walk: full path command parser (MoveTo, LineTo, CubicTo, Close), de Casteljau cubic flattening, triangle fan generation from centroid, covering quad for stencil test pass
- Protocol: stencil DSA states (write: front INCR_WRAP / back DECR_WRAP; test: NOTEQUAL with ZERO reset), no-color blend, stencil ref command, stencil clear
- Driver: stencil pipeline setup separated into own SUBMIT_3D (can fail independently); `stencil_available` flag gates path rendering

**Stencil blocked on virglrenderer/ANGLE:** `VIRGL_BIND_DEPTH_STENCIL` (0x100) rejected for PIPE_TEXTURE_2D resources. Error: "Buffer bind flags require the buffer target." This is an ANGLE Metal backend limitation — it doesn't expose depth/stencil as a sampler-view-compatible texture target through virglrenderer's resource creation path. The stencil-then-cover decision (2026-03-17) is architecturally correct but needs either real hardware or a CPU-rasterized fallback.

### Key bugs found and fixed

1. **Stack overflow from path flattening buffer:** `MAX_POINTS=2048` array (16 KiB) on a 16 KiB stack. Reduced to 256 (2 KiB). The 16 KiB stack constraint continues to be the #1 cause of crashes in userspace.

2. **Context poisoning from failed stencil commands:** A failed `CREATE_OBJECT` (stencil surface) inside `SUBMIT_3D` sets a context error flag, causing ALL subsequent commands in the same batch to be rejected — including critical framebuffer/viewport setup. Fix: moved stencil setup to a separate `SUBMIT_3D` call that can fail independently.

3. **VBO aliasing with deferred command submission:** Image vertices and glyph vertices both wrote to `TEXT_VB_RESOURCE_ID`. With two `submit_3d` calls per frame, the glyph upload overwrote image vertices before the GPU could draw them. This caused the image to flicker — appearing and disappearing every 1-5 seconds (whenever QEMU's display thread scanned out between submissions). Fix: pack image vertices at offset 0 and glyph vertices at offset 192 in the same VBO, upload once, and draw both with different `cmd_set_vertex_buffers` offsets in a single `SUBMIT_3D`.

4. **Virgl bind flag divergence:** `VIRGL_BIND_DEPTH_STENCIL = 0x100` (bit 8), not `PIPE_BIND_DEPTH_STENCIL = 0x04` (bit 2). Most other bind flags match between virgl_hw.h and Gallium p_defines.h (RENDER_TARGET=0x02, SAMPLER_VIEW=0x08, VERTEX_BUFFER=0x10).

### Architecture note: single SUBMIT_3D per frame

The render loop now uses exactly one `SUBMIT_3D` per frame containing all draw commands (backgrounds, images, glyphs). VBO aliasing is solved by packing different vertex arrays at different offsets within the same buffer resource, then binding with the appropriate byte offset before each draw call. This eliminates intermediate-state flickering caused by QEMU's display thread scanning out between submissions.

### Path rendering: fallback options

The stencil-then-cover approach is blocked on the virgl/ANGLE stack but the code is ready for real hardware. For the prototype, two fallback options:

1. **CPU-rasterized path → texture upload:** Rasterize paths to R8 coverage bitmap on CPU (the render library has a working scanline rasterizer), upload as texture, composite with GLYPH_FS. Same approach as glyph rendering but with path-derived coverage.

2. **Triangle rendering without stencil:** For convex paths (rectangles, simple shapes), direct triangle rendering works without stencil. Only concave/self-intersecting paths need stencil winding.

---

## GPU Path Rendering: Stencil-Then-Cover vs Compute (2026-03-17)

**Status:** research complete, approach settled, stencil-then-cover blocked on ANGLE (see 2026-03-18 entry above).

### The question

How should the GPU render filled vector paths (cubic Bezier contours)? Three approaches evaluated:

### Approaches considered

**A. CPU rasterize → texture upload.** Rasterize path to coverage bitmap on CPU, upload as GPU texture, composite as textured quad. What Cairo and older Skia did. Defeats the purpose of GPU acceleration — CPU does the hard work, GPU just blits. Also requires per-path texture resource management.

**B. Tessellate → triangles.** Flatten Bezier curves to line segments, triangulate the filled polygon, draw as colored triangles. Problem: correct triangulation of arbitrary concave, self-intersecting polygons with holes is a hard computational geometry problem (ear clipping is O(n²), constrained Delaunay is complex). Doesn't handle winding rules naturally.

**C. Stencil-then-cover.** The dominant production technique — used by NanoVG, Pathfinder, Skia (Ganesh/Graphite), Chrome, Firefox (WebRender), Direct2D. Algorithm:

1. Triangle fan from an arbitrary interior point into the **stencil buffer** (increment/decrement tracks winding count) — overlapping triangles are fine
2. Covering quad reads stencil buffer — non-zero stencil (or odd for even-odd rule) gets filled

Handles self-intersections, complex winding rules, and concave shapes correctly without correct triangulation. Requires only a stencil buffer (standard since ES 2.0).

**D. Compute shader (Vello-style).** GPU-native path rendering via compute shaders. CPU flattens curves → SSBO → compute shader does scanline coverage → output texture. No stencil, no tessellation. The frontier (Raph Levien / Linebender project, 2024-2025).

### Decision: Stencil-then-cover (C) for current driver, compute (D) for future hardware drivers

**Stencil-then-cover** is the right approach for virgil-render:

- Works within ES 3.0 (our ceiling — ANGLE/Metal hardcodes `kMaxSupportedGLVersion = gl::Version(3, 0)`)
- virglrenderer supports stencil buffers (standard Gallium3D)
- Battle-tested in production by every major renderer
- Implementation: depth/stencil surface (one-time), two passes per path

**Compute is blocked** on the current QEMU stack:

- Requires ES 3.1 (compute shaders) + SSBOs
- ANGLE's Metal backend explicitly caps at ES 3.0, sets `maxPerStageStorageBuffers = 0`
- virglrenderer's protocol _does_ support compute (`VIRGL_CCMD_LAUNCH_GRID`, `SET_SHADER_BUFFERS`, `SET_SHADER_IMAGES`, `MEMORY_BARRIER`, `PIPE_SHADER_COMPUTE`), but the host-side GL doesn't

**Compute becomes viable on real hardware:** When a native GPU driver replaces virglrenderer (no ANGLE translation layer), every modern GPU (Apple M-series, AMD RDNA, Intel Arc, Arm Mali) has compute as a fundamental capability. The thick driver architecture makes this a driver-internal choice — same scene graph interface, different rendering strategy.

### Key insight: the prototype validates the interface, not the rendering technique

The scene graph is the stable boundary. Stencil-then-cover on QEMU proves paths work through the pipeline. Compute on real hardware would be a performance upgrade behind the same interface. Both are invisible to core, editors, and the rest of the OS.

### Implementation plan (when paths are needed)

1. Add a depth/stencil renderbuffer to the framebuffer setup in `setup_pipeline`
2. Create a DSA state with stencil test enabled (increment on front-face, decrement on back-face)
3. Per path: triangle fan from centroid → stencil pass (no color write), then covering quad → stencil test + color write
4. Reset stencil to zero between paths

Not implementing now — `Content::Path` exists in the scene graph enum but core never emits it. The Phase 2 rendering redesign (2026-03-16) eliminated SVG paths; icons use the glyph cache.

---

## GPU Rendering Architecture: Thick Drivers (2026-03-17)

**Status:** settled design, ready to implement.

### The decision

GPU drivers are **thick**: they read the scene graph from shared memory, perform the full tree walk, and produce pixels using hardware-accelerated rendering. The scene graph is the interface between the OS service (core) and the GPU driver. There is no intermediate command list or display list abstraction.

### Architecture

```text
Core (layout + scene build) → Scene Graph (shared memory) → GPU Driver (tree walk + render + present) → Display
```

The current pipeline splits rendering across two processes:

```text
Core → Scene Graph → Compositor (CpuBackend tree walk + rasterization) → virtio-gpu 2D driver (dumb blit) → Display
```

The new architecture collapses compositor + GPU driver into a single **render service** process per display backend:

```text
Core → Scene Graph → virgil-render (tree walk + Virgil3D commands + present) → Display
Core → Scene Graph → cpu-render (tree walk + software rasterization + present) → Display
```

Init selects which render service to launch. Only one runs at a time per display.

### Why thick drivers

The key question was where to put complexity in the `compositor → driver` pipeline. Three options were evaluated:

1. **Thin driver (current).** Compositor does all rendering, GPU driver is a dumb blitter. Works for CPU rendering but means a GPU-accelerated path would still do all rendering in the compositor, then serialize GPU commands across a process boundary. Cross-process command buffer protocol adds a new interface in the hot path.

2. **Hybrid: command list intermediate representation.** Tree walk emits a flat command list (fill rect, blit glyph, blend surface), backends execute it. Shares the tree walk across backends, but the command list becomes a committed interface — a mini graphics API in the middle of the pipeline. If it needs to change (new content type, GPU-specific optimization), every backend and the tree walk layer must update. This is complexity in the connective tissue, which violates the design principle: connective tissue must be simple.

3. **Thick driver (chosen).** Each driver reads the scene graph directly and does whatever it needs. The scene graph is the only interface. Drivers are leaf nodes — complex inside, simple boundary. Duplication of tree walk logic across drivers is mitigated by a shared utility library when needed (not before a second driver exists).

The thick driver approach was chosen because:

- **The scene graph is the interface.** It's already designed, already in shared memory, already stable. No new interface to design or maintain.
- **Complexity at the edges.** Essential rendering complexity (tree walk, clipping, batching, hardware commands) lives inside the driver — a leaf node behind the scene graph interface. This is exactly where "push complexity to the edges" says it should go.
- **No connective tissue coupling.** A command list in the middle would couple all backends to a shared format. Thick drivers are independent — a Virgil3D driver and a future Metal driver don't need to agree on anything except the scene graph.
- **GPU-specific optimization without abstraction leaks.** A GPU driver can batch glyphs, reorder draws, use native texture atlases — all internal decisions. A command list would either be too simple (preventing optimizations) or too complex (leaking GPU concerns upward).

### What about code duplication?

Multiple thick drivers would duplicate tree walk and clipping logic. Mitigations:

- **Don't solve it prematurely.** Today there's one GPU driver (Virgil3D) and one CPU fallback. Extract shared code when a second GPU driver appears, not before.
- **Shared utility library (`render-util`) when needed.** Common building blocks (tree walk skeleton, clipping math, coordinate scaling, glyph atlas management) as a toolkit — composable functions, not a framework. Each driver picks what it needs.
- **The render library (`libraries/render/`) already exists.** Its `scene_render.rs` (tree walk, clipping, compositing) and utility modules can be reused by any driver that wants them. The `CpuBackend` is just one consumer.

### Visual effects are node properties, not content types

Shadows, blur, opacity, transforms, and rounded corners are properties on `Node`, not `Content` variants. This is correct: they're visual modifiers derived from the node's shape, not independent content. A thick driver renders these properties however it wants — CPU does box blur in software, GPU does it with a shader. The scene graph interface doesn't change.

Content types remain: `None`, `Path`, `Glyphs`, `Image`.

### Terminology

- **Render service** — a process that reads the scene graph and produces display output. Encompasses tree walk, rendering, and hardware presentation.
- **GPU driver** — a render service backed by GPU hardware. "Driver" conveys hardware abstraction; "thick" conveys the higher abstraction level of the interface (scene graph vs draw calls).
- **CPU render** — the existing `CpuBackend` path, restructured as a standalone render service (merging the current compositor + virtio-gpu 2D driver).

### Implementation plan (Virgil3D render service)

**Phase 1: Virgil3D driver scaffolding.**
Create `services/drivers/virgil-render/`. Initialize virtio-gpu in 3D mode (Virgl context). Establish a rendering context that can submit Gallium3D commands to the host GPU via virtio-gpu capsets. Verify basic operation: clear screen to a solid color.

**Phase 2: Scene graph rendering.**
Read the scene graph from shared memory (same `TripleReader` the compositor uses). Implement tree walk: `Path` → tessellate to triangles + GPU fill (or stencil-then-cover), `Glyphs` → texture atlas + textured quads, `Image` → texture upload + blit. Handle clipping, transforms, opacity. Present to display.

**Phase 3: Glyph atlas.**
Upload rasterized glyphs to a GPU texture atlas. The fonts library rasterizes (CPU-side, same as today); the driver uploads coverage bitmaps to GPU memory and composites them via textured quads or blits.

**Phase 4: Init integration.**
Init selects the render service at boot (Virgil3D if available, CPU fallback otherwise). The scene graph shared memory setup is identical for both paths — init doesn't know or care which driver reads it.

**Phase 5: CPU render service restructure.**
Merge the existing compositor + virtio-gpu 2D driver into a single `services/drivers/cpu-render/` process — sibling to `virgil-render/`. Same shape, same interface (reads scene graph, produces display output). The current `services/compositor/` and `services/drivers/virtio-gpu/` are then deleted — no parallel implementations. The render library (`libraries/render/`) and its `CpuBackend` move into the new process.

```text
services/drivers/
  cpu-render/       (merges compositor + virtio-gpu 2D)
  virgil-render/    (thick Virgil3D driver)
```

### Unforeseen issues audit

Reviewed potential problems with the thick driver approach:

- **Video playback:** `Image` nodes in shared memory, driver composites via texture upload. Actually better than thin drivers — decoder and renderer in same process, zero-copy possible.
- **Hardware video decode:** GPU often does decoding too. Thick driver has both decoder and renderer — no cross-process transfer.
- **Shader effects:** New node properties (blur, shadow already exist), not new content types. Driver implements however it wants.
- **Multiple displays / mixed DPI:** Each display gets its own driver instance reading the same scene graph, rendering at its own scale. Works naturally.
- **Multiple GPUs simultaneously:** Scene graph is in shared memory, multiple drivers can read it. No issue.
- **Something above the scene graph needing direct GPU access:** Architecture prevents this. Editors don't render. Core builds the scene graph. The scene graph is expressive enough to add new visual operations via new content types or node properties.

No architectural problems identified.

---

## Text Shaping Pipeline: HarfBuzz Integration (2026-03-17)

**Status:** Complete. Core calls `fonts::shape_with_variations()` via `shape_text()`. The fake monospace shaper (`bytes_to_shaped_glyphs`) remains in test code only. The open items below document the original analysis and remaining edge cases.

### The problem

There are **two `ShapedGlyph` types** that don't talk to each other:

1. **`fonts::ShapedGlyph`** (shaping library output): `glyph_id: u16`, `x_advance: i32`, `y_advance: i32`, `x_offset: i32`, `y_offset: i32`, `cluster: u32`. Values in **font units** (design units, an arbitrary grid per font — typically 1000 or 2048 units per em). Produced by `fonts::shape()` / `fonts::shape_with_variations()` which wrap HarfBuzz (harfrust). This is the real shaping pipeline.

2. **`scene::ShapedGlyph`** (scene graph wire format): `glyph_id: u16`, `x_advance: i16`, `x_offset: i16`, `y_offset: i16`. Values in **points** (the scene graph's logical coordinate unit, 1pt = 1/110"). 8 bytes total, `#[repr(C)]`. This is what the render backend reads.

Core previously bypassed `fonts::shape()` entirely, using `bytes_to_shaped_glyphs(text, advance)` — a fake shaper that treated each ASCII byte as a glyph ID. This was replaced by `shape_text()` in `scene_state.rs`, which calls `fonts::shape_with_variations()` (HarfBuzz via harfrust), converts font units to pixel units, and produces `scene::ShapedGlyph` values for the scene graph. The fake shaper remains in test code only.

### Unit chain

```text
font units × (point_size / units_per_em) = points
points × scale_factor = physical pixels
```

- **Font units → points**: Core's responsibility. Core calls the shaper, gets font units, converts to points using the font's `units_per_em` and the requested point size.
- **Points → physical pixels**: Render backend's responsibility. The backend reads `scene::ShapedGlyph` values in points and multiplies by the display scale factor.

Currently the scene graph comments say "scaled pixel units" — this should say "points" to match the settled terminology (journal entry on logical unit definition).

### What needs to change

**1. Core calls `fonts::shape_with_variations()` instead of `bytes_to_shaped_glyphs()`.**

Core already has the font data in shared memory and the axis values (MONO=1). It needs to:

- Call `fonts::shape_with_variations(font_data, text, features, axes)`
- Convert the returned `fonts::ShapedGlyph` array from font units to points: `value_pt = value_fu * point_size / units_per_em`
- Write the results as `scene::ShapedGlyph` into the scene graph data buffer

**2. `scene::ShapedGlyph` field widths may need revisiting.**

Currently `i16` for advances/offsets. At point size 18 with upem=1000, a typical advance is ~10pt, which fits easily. But at larger point sizes or with fonts that have large design units, `i16` (max 32767) could overflow. Check: `max_advance_fu * max_point_size / min_upem` — if this exceeds 32767, widen to `i32`. The tradeoff is glyph data size in the scene graph data buffer (8 bytes vs 14 bytes per glyph).

At point size 18, upem 1000, max reasonable advance ~600fu: `600 * 18 / 1000 = 10.8pt`. Even at point size 72: `600 * 72 / 1000 = 43.2pt`. `i16` is fine for any practical point size. Keep `i16`.

**3. The clock's in-place update path needs rethinking.**

Currently `update_clock` / `update_clock_inline` overwrite glyph data in-place, assuming the new text produces the same number of glyphs with the same byte layout. With real shaping this assumption breaks — different clock strings could produce different glyph counts (ligatures) or different advances (kerning pairs like "1:" vs "2:"). The in-place path should be replaced with a re-shape + re-push into the data buffer, or the clock update should go through `update_document_content` which already re-pushes all data.

**4. `bytes_to_shaped_glyphs` is deleted.**

It's the fake shaper. Once core calls real shaping, it has no purpose.

**5. `fonts::ShapedGlyph` may be unnecessary as a separate type.**

If core converts font units to points immediately after shaping, the intermediate `fonts::ShapedGlyph` is just a transient. The shaping function could return `scene::ShapedGlyph` directly (with the conversion baked in) if we pass `point_size` and `upem` to the shaping call. Or keep them separate to maintain the library boundary — the fonts library shouldn't depend on the scene library. Keeping them separate is cleaner: `fonts` is a pure shaping/rasterization library, `scene` is the IPC wire format.

### What doesn't change

- The render backend reads `scene::ShapedGlyph` from the data buffer and uses `x_advance` (in points) to position glyphs. It multiplies by scale to get physical pixels. This is correct.
- The glyph cache in the render backend is keyed by glyph ID and rasterizes at physical pixel size. This is correct — it doesn't care about advances.
- The scene graph's `Content::Glyphs` node type carries a `DataRef` to the shaped glyph array. This is correct.
- Core's scene-building functions (`build_editor_scene`, `update_document_content`) already re-push glyph data on each call. They just need to call the real shaper instead of the fake one.

### Why not fix it now

The monospace fake shaper works for the prototype. The interesting design questions (document model, compound documents, editor protocol) don't depend on text shaping. Fixing it is straightforward but touches the core→scene data flow at every text-producing call site. Better to do it as a focused session when: (a) proportional text is needed, (b) non-ASCII text is needed, or (c) the clock kerning hack becomes untenable.

### Dependency: font data access in core

Core needs font data bytes to call `fonts::shape_with_variations()`. It currently has font data mapped in shared memory (loaded via 9p in init, mapped into core's address space). This is sufficient. The `units_per_em` value should be cached at startup alongside `CHAR_W` and `LINE_H` so it doesn't need to be re-parsed on every frame.

---

## Framebuffer Stale-Buffer Bug: Remove Damage Tracking from Compositor (2026-03-17)

**Status:** FIXED. Damage tracking removed from compositor, always full repaint. Serial interleaving and clock kerning also fixed in the same commit. 1,786 tests pass, QEMU visual verified.

### The bug

After the triple buffering mission completed, the display flickers every clock tick: the titlebar, document title, cursor, and trailing text characters appear and disappear each second. One screenshot shows the full UI (titlebar with "Text", clock, cursor). The next shows only the clock on a black background. The clock is always visible because it's the only dirty region being re-rendered each tick.

### Root cause: double-buffered framebuffers with no copy-forward

The compositor double-buffers its pixel framebuffers (`fb_va`, `fb_va2`). On each frame it renders into `1 - presented_buf` (the non-displayed buffer), then presents and swaps. When the render backend does a **partial update** (only dirty rectangles), it only repaints those regions into the target framebuffer. The rest of that framebuffer still contains whatever was last rendered into it — which was **two frames ago**, not one.

Timeline:

1. Frame N: full repaint into FB0. Present FB0. `presented_buf = 0`.
2. Frame N+1 (clock tick): partial update, renders only the clock rect into FB1. But FB1 still has stale content from frame N-1 (or is black from boot). Titlebar, text, cursor are not in the dirty region, so they're never painted. Present FB1.
3. Frame N+2 (clock tick): partial update into FB0. FB0 still has the full scene from step 1. Everything is visible. Present FB0.
4. Repeat: alternating between a complete FB0 and an incomplete FB1.

The scene graph's triple-buffer `acquire_copy` correctly copies scene data forward before mutation. But the compositor's framebuffer double-buffer has no equivalent copy-forward step. The discipline that makes triple buffering correct was never applied to the pixel output layer below it.

### Design discussion: should we fix damage tracking or remove it?

**Game engines vs. compositors.** Game engines (Unity, Unreal, Vulkan apps) never do partial updates — every frame is a full redraw. The GPU fills pixels so fast that damage tracking isn't worth the complexity. Desktop compositors (Wayland/Weston, macOS Core Animation, Android SurfaceFlinger) do partial updates because compositing multiple overlapping windows with alpha blending, blur, and shadows is expensive, and 99% of frames only change a tiny region.

**Where this system sits.** Right now: a single full-screen document, a titlebar, a cursor, a clock. CPU software rasterizer. QEMU runs at the host Mac's physical resolution (3456x2234 on the development machine — the run script detects this via `system_profiler` and passes it to virtio-gpu). At that resolution, each framebuffer is ~30.8MB and full repaints touch 7.7 million pixels. Damage tracking would meaningfully help the CPU path at this resolution — a clock tick touching ~200 pixels vs 7.7M is a 38,000x difference. But the damage tracking is in the wrong layer (compositor, not render backend), causing the stale-buffer bug.

**The CPU rasterizer is temporary.** virtio-gpu with Virgil 3D provides GPU-accelerated rendering inside QEMU. The architecture should assume a GPU backend as the primary path, with the CPU rasterizer as a fallback. A GPU backend would do full repaints trivially (like game engines). Damage tracking only makes sense as an optimization internal to the CPU fallback path — not in the compositor's render loop where it creates layer-boundary bugs.

**Damage tracking was in the wrong layer.** The compositor shouldn't decide whether to skip pixels — it should always ask for a full render. If a `CpuBackend` internally wants to optimize by tracking damage and doing copy-forward on its own managed buffers, that's its business. The compositor's framebuffer management doesn't support partial updates correctly (no copy-forward), which is the root cause of the bug.

### Decision: remove damage tracking entirely for now

1. **Compositor always does full repaints.** Read the scene, call `backend.render()`, present. No skip logic, no dirty rect tracking, no `FrameAction::Partial`.
2. **`RenderBackend` trait simplifies.** Remove `prepare_frame`, `finish_frame`, `update_bounds_for_skip`, `dirty_rects`, `FrameAction`. The trait is just `render()`.
3. **`CpuBackend` strips damage internals.** Remove `damage: DamageTracker`, `prev_bounds`, `prev_node_count`. Keep glyph caches, scale, surface pool.
4. **`damage.rs` module stays in the tree.** It's a standalone building block for future use — either inside a future `CpuBackend` optimization or for a compositor that properly manages its own buffer copies.
5. **GPU driver receives full-screen transfers.** Always `rect_count = 0`. The partial transfer path stays in the GPU driver (it's correct and tested) but won't be exercised until damage tracking is reintroduced at the right layer.
6. **Scene graph change list stays.** Core's incremental updates (`update_clock`, `update_cursor`, `mark_changed`, `acquire_copy`) are still valuable for minimizing scene _building_ work. That's independent of how the compositor _renders_ it.

### When to reintroduce damage tracking

At 3456x2234, full CPU repaints are expensive enough that damage tracking will likely be needed soon. But the reintroduction should be:

- Inside the `CpuBackend`, not the compositor — the backend manages its own buffer copies and damage state
- With proper copy-forward: the backend maintains its own previous-frame buffer and copies it to the render target before partial updates
- Or not at all if a `GpuBackend` handles the workload (GPU full repaints are effectively free)

### Design for GPU, fall back to CPU

The `RenderBackend` trait supports this cleanly:

- **`GpuBackend`:** full repaints every frame, no damage tracking, GPU handles it trivially
- **`CpuBackend`:** could internally optimize with damage tracking + copy-forward, because CPU pixel-filling is expensive enough to justify the complexity
- Damage tracking becomes a backend-internal optimization, not a compositor-level protocol

---

## Scene Graph Content Type Revision: Path + Subpixel Removal + FillRect Removal (2026-03-17)

**Status:** design-settled, not yet implemented (blocked by in-progress transport bugs mission).

### Context

The Phase 2 rendering redesign (2026-03-16) replaced semantic content types (`Text`, `Path`) with geometric ones (`FillRect`, `Glyphs`, `Image`). This eliminated the SVG parser and routed icons through the glyph cache. But the redesign left a gap: the render backend already IS a path rasterizer (Bezier flattening + scanline coverage for font outlines), yet the scene graph has no way to express arbitrary vector content. Editors would have to pre-rasterize vector content to `Image`, splitting rasterization across both sides of the interface and losing resolution independence.

### Decisions

**1. Add `Path` content type.** Arbitrary filled Bezier contours as a first-class scene graph primitive. The render backend rasterizes them with the same scanline engine it uses for glyphs. No editor does its own pixel work. Vector content scales cleanly at any display density.

```rust
Path {
    color: Color,
    fill_rule: FillRule,          // Winding | EvenOdd
    contours: DataRef,            // cubic Bezier commands in data buffer
    stroke: Option<StrokeParams>, // width, cap, join — backend handles expansion
}
```

**2. ~~Remove `FillRect` content type.~~** **Done.** `FillRect` removed from `Content` enum. Solid rectangles use `Content::None` with `node.background` color. Cursor and selection highlights use this pattern. Tests validate (`content_enum_has_three_variants`, `core_cursor_uses_background_container`).

**3. Cubic Beziers only at the interface.** The scene graph mandates one contour format: cubic Beziers. Quadratics (TrueType font outlines) are converted to cubics losslessly when Core builds the scene graph (`cp1 = p0 + 2/3*(c-p0)`, `cp2 = p1 + 2/3*(c-p1)`). One format, one flattening algorithm in the backend. If the rasterizer wants to detect degenerate cubics and fast-path them as quadratics internally, that's its business — implementation detail behind the interface.

**4. Logical coordinates, not pixels.** Core does layout in device-independent logical units. The scene graph carries logical coordinates. The render backend multiplies by a display scale factor to get physical pixels for rasterization. Path contour coordinates are in the same logical space as node x/y/width/height. This keeps the entire pipeline above the render backend density-agnostic — Core doesn't know or care whether it's 1x or 2x. What "logical unit" means precisely (points, CSS px, abstract scene units) is a scene-graph-wide decision, not Path-specific.

**5. Stroke in the render backend.** The render backend handles stroke expansion (offset curves + caps + joins) internally. Stroke parameters (width, cap style, join style) live on the Path node as optional fields. This follows the principle of pushing complexity to the leaf nodes — the render backend is the leaf. Centralizing stroke logic in the backend avoids duplicating it across editors.

**6. Remove subpixel (LCD) rendering.** macOS dropped it in 2018. Android/iOS never used it on HiDPI. The OS targets Retina-class displays (220+ PPI) where individual pixels are below the eye's resolving power. Subpixel rendering adds complexity (color fringing, display-technology dependence) for imperceptible benefit at HiDPI. Switch to grayscale antialiasing with vertical oversampling only. This simplifies the rasterizer and eliminates the only remaining difference between glyph rasterization and general path rasterization.

### Content types after this revision

- `None` — pure container (decoration via background_color, border, corner_radius, opacity)
- `Path` — filled (and optionally stroked) cubic Bezier contours
- `Glyphs` — batched font glyph run (glyph IDs + advances + font reference). Distinct from Path because text is a fundamentally different data shape: batched references with shaping metadata, not inline contours. The render backend uses the same rasterizer internally.
- `Image` — pixel data reference

### Why Glyphs stays separate from Path

A page of text has ~5,000 characters but currently ~50 scene graph nodes (one `Glyphs` per line, carrying an array of glyph_id + x_advance). Unifying Glyphs into Path would either: (a) emit 5,000 individual Path nodes (massive scene graph bloat), or (b) require a batching mechanism — an array of (PathId, position) pairs in one node — which is exactly what Glyphs already is. The distinction is batched-references-with-shaping-metadata vs. inline-contours. These are different data shapes at the interface, not an optimization leak.

### Caching

Caching is an implementation detail behind the interface. The render backend can cache rasterized coverage maps for any contour, keyed by (contour identity, scale). For `Glyphs`, contour identity comes from (glyph_id, font). For `Path`, it can come from the DataRef offset or a contour hash. Multiple scene graph nodes can reference the same DataRef in the data buffer (e.g., repeated shapes). The scene graph expresses what to render; the backend decides what to cache.

### Relationship to rendering pipeline architecture

The settled pipeline remains:

```text
Core (shaping, layout, scene building) → Scene Graph (shared memory) → Compositor (thin event loop) → Render Backend (tree walk, rasterization, compositing) → GPU Driver → Display
```

Path doesn't change any component boundaries. Core writes contour data into the scene graph. The render backend rasterizes it. The compositor never sees or cares about content types.

### Resolved: Logical unit definition (points)

The scene graph's logical unit is a **point**, matching the macOS convention: 1pt = 1/110" at the reference density. The render backend applies `scale = physical_PPI / 110` to convert to physical pixels (~2.0 on Retina, 1.0 on QEMU's virtual framebuffer). Core, editors, and the scene graph never deal in physical pixels — the entire pipeline above the render backend is density-agnostic.

Font sizes, node positions, path coordinates all use the same unit. "Font size 16" = 16 points. A 200pt-wide panel = 200 points. The only thing that changes between displays is the scale factor.

Why not viewport-relative (vh/vw): a point-based unit maintains roughly consistent perceived size across display sizes and densities. Viewport-relative units make content tiny on small screens and huge on large ones — they solve the aspect ratio problem but create a sizing problem. Every modern system (macOS pt, iOS pt, Android dp, CSS px) converges on density-based logical units with per-device scale factors for this reason.

### Resolved: Path command encoding

**Commands:** four cubic Bezier commands — MoveTo, LineTo, CubicTo, Close. Same as SVG/PostScript/Skia/Cairo. Higher-level constructs (arcs, rounded rects) decompose to these in Core before the scene graph.

**Coordinates:** f32. Sub-point precision for smooth curves. Natural for AArch64 (full FPU). IEEE 754, well-defined in shared memory. The rasterizer converts to its internal integer math during Bezier flattening anyway. No benefit to fixed-point when hardware has an FPU.

**Layout:** variable-size commands, sequential in the data buffer. Referenced by a `DataRef` on the Path node.

```text
MoveTo:  [tag: u32, x: f32, y: f32]                                          = 12 bytes
LineTo:  [tag: u32, x: f32, y: f32]                                          = 12 bytes
CubicTo: [tag: u32, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32]  = 28 bytes
Close:   [tag: u32]                                                          =  4 bytes
```

Variable-size is compact and matches access patterns — the rasterizer walks commands sequentially for flattening, never needs random access by index. u32 tag ensures 4-byte alignment for all f32 fields.

---

## Rendering Pipeline Transport Bugs (2026-03-17)

**Status:** FIXED. All six bugs fixed via triple buffering + GPU completion flow control. TripleWriter/TripleReader replace DoubleWriter/DoubleReader with mailbox semantics (acquire always succeeds, reader gets latest). MSG_PRESENT_DONE adds GPU→compositor backpressure. Dirty rect coalescing unions all rects. Damage tracking handles skipped frames. 23 new tests, 1,791 total pass. QEMU visual + 68s stress test verified.

**Original problem:** Six related issues across the rendering pipeline, all stemming from unsynchronized shared-memory transport between pipeline stages. No backpressure, no flow control. The pipeline assumes each stage is faster than the previous one and never falls behind. When that assumption breaks: dropped frames, torn reads, stale data.

The pipeline architecture (data shapes and translators) is sound. The problems are all in the _transport_ between translators.

### Bug 1: Scene graph double-buffering drops frames (core → compositor)

The scene graph uses double buffering in shared memory between core (writer) and compositor (reader). When the writer gets 2+ frames ahead of the reader, there is no safe buffer to write to — the front is what the reader will read, and the back may be what the reader is currently reading. `copy_front_to_back()` correctly detects this and returns `false`, but the scene update is then silently dropped. The character is in the document but never rendered until a future event produces a successful `copy_front_to_back`.

The `reader_done_gen` check is conservative by design: it tracks the last generation the reader _finished_, not what it's _currently_ reading. When `reader_done_gen < back_gen`, the back buffer might be in use. This is correct — the alternative is torn reads.

With two buffers, the writer can get exactly one frame ahead. Any further writes require the reader to have finished. This is a fundamental constraint of the protocol, not a bug in the implementation. The failure manifests under fast typing because core processes input → editor → document mutation → scene build faster than the compositor renders the previous frame.

Attempted fixes that failed:

- **Spin-loop (`wait_for_back`):** Blocks core until compositor finishes. Violates "event-driven over polling." Can deadlock if core and compositor share a CPU core.
- **Retry timer (2ms):** Event-driven but adds complexity and doesn't prevent the initial dropped frame. Still drops the first attempt; latency depends on timer granularity.
- **Always render immediately in compositor:** Removed valid frame coalescing without fixing the root cause.

**Files:** `libraries/scene/lib.rs` (double-buffer protocol), `services/core/scene_state.rs` (all `copy_front_to_back` call sites return early on failure), `services/core/main.rs` (scene dispatch silently drops failed updates).

### Bug 2: Framebuffer tearing (compositor → GPU driver)

The compositor has `presented_buf` toggling between framebuffers 0 and 1. On full repaint it renders to `1 - presented_buf` (the non-displayed buffer), then presents — correct double-buffer usage. But on partial update it renders _into the currently displayed buffer_ (`presented_buf`). There is no synchronization with the GPU driver — the compositor writes pixels to a framebuffer while the GPU driver may be in the middle of `transfer_to_host` reading those same pixels. This is tearing. It works visually because virtio-gpu on QEMU is fast enough that the race window is small, but it's the same class of problem as Bug 1: unsynchronized shared memory between producer and consumer.

**Files:** `services/compositor/main.rs` (lines 153-161, the `presented_buf` logic), `services/drivers/virtio-gpu/main.rs` (present loop reads framebuffer without synchronization).

### Bug 3: Compositor frame scheduling can defer scene updates

When core signals a scene update, the compositor calls `should_render_immediately()`, which returns `true` only if the last timer tick was more than half a frame period ago. Otherwise it sets `dirty = true` and waits for the next timer tick. If core writes another scene update before the tick fires, the scene data in shared memory is overwritten. The first update's frame was never rendered.

This is usually fine — only the latest state matters for rendering. But combined with Bug 1, it means the compositor can defer rendering a frame that then gets overwritten before it's ever read, compounding the dropped-frame problem.

**Files:** `services/compositor/frame_scheduler.rs` (`should_render_immediately`), `services/compositor/main.rs` (line 130).

### Bug 4: GPU driver present coalescing loses dirty rects

The GPU driver drains all pending `MSG_PRESENT` messages and uses the _last_ one (`last_payload`). If the compositor sends two presents with different dirty rects before the GPU driver wakes up, only the last set of dirty rects is transferred to the host. The pixels from the first present's dirty region are not transferred, causing partial screen corruption until the next full repaint.

**Files:** `services/drivers/virtio-gpu/main.rs` (present loop around line 831 — "coalesce: use the last one").

### Bug 5: No backpressure from GPU to compositor

The compositor sends `MSG_PRESENT` and immediately continues to the next frame. It never waits for the GPU driver to finish the transfer. If the compositor produces frames faster than the GPU can transfer, presents pile up in the IPC ring buffer and get coalesced (see Bug 4). There's no flow control signal. In a real GPU pipeline you'd have a fence or semaphore — the compositor waits until the GPU is done with a framebuffer before writing to it again.

**Files:** `services/compositor/main.rs` (present is fire-and-forget), `services/drivers/virtio-gpu/main.rs` (no completion signal back to compositor).

### Bug 6: Damage tracking uses stale bounds after skipped frames

`PREV_BOUNDS` in the render backend stores the previous frame's node positions so the damage tracker can mark both old and new positions as dirty when a node moves. But if a frame is skipped (due to Bug 1 or Bug 3), `PREV_BOUNDS` doesn't update, so the next frame's damage calculation uses stale bounds. This can cause rendering artifacts — old content not cleared, or new content not drawn in the right region.

**Files:** `libraries/render/lib.rs` (`prev_bounds` array, `finish_frame` method).

### The structural fix: triple buffering + flow control

All six bugs are instances of the same pattern: unsynchronized producer-consumer shared memory with no backpressure. The fix is the same at each interface boundary.

**Triple buffering** adds a third buffer so the writer always has a free buffer. The protocol becomes:

- **Writer:** acquire the free buffer, write, publish (atomically make it the latest). Never blocks, never fails.
- **Reader:** always read the most recently published buffer. Intermediate frames are silently skipped (the reader always sees the latest state).
- **The third buffer** is whichever one neither the writer nor reader is currently using.

The writer never blocks, the reader never sees torn data, and frame skipping is the natural behavior when the writer is faster (which is correct — only the latest state matters for rendering).

**Flow control** (fences/semaphores) at the compositor → GPU boundary ensures the compositor doesn't write to a framebuffer the GPU is still reading. The GPU signals completion; the compositor waits for it before reusing that buffer.

### Prior art

Triple buffering is the standard solution in graphics pipelines:

- **Android (SurfaceFlinger):** Triple buffering by default since 4.1 (Project Butter, 2012). `BufferQueue` manages a pool of typically 3 buffers.
- **Wayland/Weston:** `wl_surface` protocol designed around multi-buffer attach/commit.
- **macOS (Core Animation):** Triple-buffered internally via CALayer buffer pools.
- **Windows (DWM/DXGI):** Desktop Window Manager uses triple-buffered composition. DXGI swap chains default to 2-3 buffers.
- **Fuchsia (Scenic):** `BufferCollection` with configurable count, typically 3.
- **Vulkan/Metal/DX12:** All expose explicit swap chain buffer counts; 3 is the standard recommendation.

### Presentation model: mailbox, not FIFO

Triple buffering has two flavors (see [Triple Buffering in Rendering APIs](https://www.4rknova.com/blog/2025/09/12/triple-buffering)):

- **FIFO (queue):** Frames are consumed in order. Every frame produced is eventually displayed. Latency increases under load because frames queue up. Correct for video playback and sequential animation where every frame matters.
- **Mailbox (flip):** The writer publishes to a slot; the reader always takes the latest. Intermediate frames are silently replaced. No latency buildup. Correct for interactive UI where only the latest state matters.

**The scene graph transport should use mailbox.** The scene represents the current document state. If core produces three updates before the compositor renders, the compositor should see the latest, not replay intermediates. Showing stale intermediate states adds latency with no benefit.

**Video and animation use FIFO, but at the leaf level, not the transport level.** A video node in the scene tree would own its own FIFO buffer pool for decoded frames. The compositor composites the latest scene tree (mailbox) but reads whichever video frame the player has marked current (FIFO, controlled by the player process via presentation timestamps). The two models compose -- they don't conflict. This is exactly how Android (SurfaceFlinger + MediaCodec), macOS (Core Animation + AVSampleBufferDisplayLayer), and Wayland (wl_surface + wp_linux_dmabuf) work.

### Implementation plan

1. ~~**Revert** all session changes (retry timer, `is_back_available`, `scene_pending` flag, `bool` return types on scene update methods). Return to clean baseline.~~
2. **Triple-buffer the scene graph** (`libraries/scene/lib.rs`). `DoubleWriter`/`DoubleReader` become `TripleWriter`/`TripleReader`. `back()` becomes `acquire()` (always succeeds), `swap()` becomes `publish()`, `copy_front_to_back()` is eliminated. Cost: ~48 KiB extra shared memory.
3. **Fix scene dispatch in core** (`services/core/main.rs`). Try incremental update, fall back to full rebuild. Both always succeed with triple buffering. No retry logic needed.
4. **Add GPU completion signal** (`services/drivers/virtio-gpu/main.rs` → `services/compositor/main.rs`). GPU driver sends `MSG_PRESENT_DONE` after transfer+flush. Compositor waits for it before reusing a framebuffer.
5. **Fix dirty rect coalescing** in GPU driver. Union the dirty rects from all coalesced presents instead of discarding earlier ones.
6. **Fix damage tracking** in render backend. When a frame is skipped (`FrameAction::Skip`), `PREV_BOUNDS` should still be valid for the next frame. Verify this is the case, or update bounds even on skip.

---

## Tickless Idle + Inter-Processor Interrupts (2026-03-16)

**Status:** Design discussion complete. Ready to implement when desired.

### Context

While evaluating what it would take to run the OS on real M1 hardware, the interrupt controller abstraction and cross-core wakeup model came into focus. The current kernel has zero IPI usage — cross-core wakeups are indirect: `try_wake` moves a thread from `blocked` to `ready` in shared scheduler state, and the destination core discovers it on its next 250 Hz timer tick (worst case 4ms). This works but leaves two gaps:

1. **Idle power waste** — all 4 cores burn cycles on 250 Hz ticks even when they have no runnable threads.
2. **Wakeup latency** — 4ms worst case is fine for interactive text editing but blocks future low-latency IPC or real-time audio.

IPIs (Inter-Processor Interrupts) are the mechanism: one core writes to a hardware register, the target core gets an interrupt immediately. On GICv3 these are system register writes to ICC_SGI1R_EL1 (no MMIO, fast). On Apple AIC (M1) they're dedicated IPI trigger registers delivered as FIQs. Different mechanism, same concept.

IPIs alone are pointless without tickless idle — if the 250 Hz tick is still running, it does the wakeup work. The real scope is tickless + IPIs together.

### GICv2 → GICv3 migration (decided)

The kernel currently uses GICv2, inherited from the reference project (`bahree/rust-microkernel`) and QEMU virt's default. There's no reason to stay on it:

- **GICv2 is a dead end.** No modern AArch64 SoC ships it. Every non-Apple ARM chip (RPi 4/5, Qualcomm, server ARM) uses GICv3. The GICv2 code has zero future utility.
- **GICv3 is strictly better.** CPU interface via system registers (`mrs`/`msr`) instead of MMIO loads/stores — faster acknowledge/EOI on the hot path. Affinity routing scales beyond 8 cores. MSI/LPI support needed for PCIe on real hardware.
- **QEMU supports it trivially.** Change `gic-version=2` to `gic-version=3`.
- **"When you build a new way, kill the old way."** Write the trait, implement `GicV3` as the only GIC backend, drop GICv2 entirely.

Key implementation differences from GICv2:

| Operation         | GICv2 (current)                 | GICv3 (target)                                                          |
| ----------------- | ------------------------------- | ----------------------------------------------------------------------- |
| Acknowledge       | MMIO read from GICC+IAR         | `mrs x0, ICC_IAR1_EL1`                                                  |
| End of interrupt  | MMIO write to GICC+EOIR         | `msr ICC_EOIR1_EL1, x0`                                                 |
| Per-core init     | MMIO writes to GICC (PMR, CTLR) | System registers: ICC_SRE_EL1, ICC_PMR_EL1, ICC_CTLR_EL1, ICC_IGRP1_EL1 |
| IRQ routing       | 8-bit CPU mask (ITARGETSR)      | 32-bit affinity (IROUTER) per SPI                                       |
| Send IPI          | MMIO write to GICD_SGIR         | `msr ICC_SGI1R_EL1, x0` (affinity encoding)                             |
| Distributor       | GICD MMIO (same base layout)    | GICD MMIO (extended) + per-core Redistributor (GICR)                    |
| DTB compat string | `arm,cortex-a15-gic`            | `arm,gic-v3`                                                            |
| MSI/LPI support   | No                              | Yes (important for PCIe — future)                                       |

The Redistributor (GICR) is the main new concept: each core has a 128 KiB MMIO region for per-core configuration (PPIs, SGIs, LPIs). Replaces what GICv2 did through banked registers.

### Design

**Tickless idle:** When a core has no runnable threads and no pending timers, it enters WFI (Wait For Interrupt) instead of spinning on a fixed tick. The timer is reprogrammed per-core to fire at the next deadline (nearest timer object or scheduler quantum expiry) rather than at a fixed 250 Hz interval.

**IPI-driven wakeup:** When `try_wake` makes a thread runnable and the target core is idle (in WFI), it sends an IPI to kick that core out of sleep. The core re-evaluates its run queue and picks up the newly-ready thread with sub-microsecond latency instead of waiting up to 4ms.

**InterruptController trait:** Abstract the GIC-specific free functions behind a trait so the kernel can target both GIC (QEMU) and AIC (M1) without conditional compilation in the scheduler/timer/interrupt forwarding code.

```rust
pub trait InterruptController {
    fn init_distributor(&self);
    fn init_per_core(&self);
    fn acknowledge(&self) -> Option<u32>;
    fn end_of_interrupt(&self, token: u32);
    fn enable_irq(&self, id: u32);
    fn disable_irq(&self, id: u32);
    fn send_ipi(&self, target_core: u32);
}
```

The current `interrupt_controller.rs` is already this shape — free functions that map 1:1 to trait methods. The refactor is mechanical.

### Current kernel state (reference)

- `interrupt_controller.rs` — GICv2 only. Free functions: `acknowledge`, `end_of_interrupt`, `enable_irq`, `disable_irq`, `init_distributor`, `init_cpu_interface`, `set_base_addresses`. No SGI/IPI support. All MMIO-based.
- `timer.rs` — Fixed 250 Hz tick (`TICKS_PER_SEC = 250`). `reprogram()` always writes `freq / 250` to CNTP_TVAL. `check_expired()` scans all timer objects on every tick. Timer PPI is IRQ 30 (per-core, doesn't route through distributor).
- `scheduler.rs` — `try_wake` / `try_wake_for_handle` move threads from blocked to ready. No IPI send. No per-core idle state tracking. `set_wake_pending` handles the case where the target thread hasn't blocked yet.
- `interrupt.rs` — Forwards device IRQs to userspace via waitable handles. Mask-on-fire, unmask-on-ack. Not affected by tickless (device IRQs are independent of the tick).
- `main.rs` — DTB parsing looks for `arm,cortex-a15-gic` (GICv2). `irq_handler` dispatches on IRQ ID (30 = timer, else forward to userspace).
- QEMU scripts (`run-qemu.sh`, `test-qemu.sh`, `test/smoke.sh`, `test/stress.sh`, `test/crash.sh`, `test/integration.sh`) — all hardcode `gic-version=2`.

### Implementation plan

**Phase 1: InterruptController trait + GICv3 (replace GICv2)**

- Define `InterruptController` trait
- Implement `GicV3` — system register CPU interface, GICD+GICR MMIO for distributor/redistributor
- Delete `interrupt_controller.rs` (GICv2 code) entirely — no parallel implementations
- Update DTB parsing in `main.rs`: look for `arm,gic-v3`, extract GICD + GICR base addresses
- Update all QEMU scripts: `gic-version=2` → `gic-version=3`
- Kernel references the trait via static dispatch
- All existing tests pass on GICv3 — functional equivalence verified

**Phase 2: Per-core idle tracking**

- Add `is_idle: bool` to per-core scheduler state
- Set `is_idle = true` when a core has no runnable threads (before WFI)
- Clear on timer tick or any interrupt that makes a thread runnable
- No behavioral change yet — just bookkeeping

**Phase 3: IPI send on wake**

- Implement `send_ipi` on GicV3 (`msr ICC_SGI1R_EL1` with target affinity encoding)
- `try_wake_impl`: after moving thread to ready queue, if target core is idle, send IPI
- Handle SGI 0 in `irq_handler` — just acknowledge it, the scheduler re-evaluation happens naturally on return from interrupt
- This alone improves wakeup latency even with the fixed tick still running

**Phase 4: Tickless idle**

- Replace fixed `reprogram(freq)` with `reprogram_next_deadline(core_id)` — computes earliest of: next timer object deadline, scheduler quantum expiry, or "no timer" (infinite sleep)
- When no deadline exists, skip timer programming entirely → core enters WFI after EOI
- WFI exit: either IPI (new work), device IRQ (forwarded normally), or timer (deadline expired)
- Remove `TICKS_PER_SEC` constant and `TICKS` counter (or keep TICKS as a debug diagnostic)

### Risks

- **SMP timing bugs.** The kernel's history includes TPIDR races, use-after-free in thread drops, and aliasing UB in syscall dispatch — all surfaced only under concurrent load. Tickless changes the timing profile and may surface latent bugs. Stress testing is mandatory at every phase.
- **Lock ordering.** IPI delivery in `try_wake_impl` happens while the scheduler lock is held. The IPI handler on the target core must NOT acquire the scheduler lock (it just returns from interrupt and re-evaluates). If the handler tried to lock, deadlock.
- **Timer reprogramming correctness.** Off-by-one in deadline calculation → missed wakeups or busy-spinning. The fixed tick is self-correcting (fires again in 4ms); tickless has no safety net.

### Interrupt controller landscape (AArch64)

Three interrupt controllers exist in the AArch64 world. The trait must support all three, but only GICv3 and AIC need implementations:

|               | GICv2 (legacy, dropping) | GICv3 (target)                | Apple AIC (M1, future)                   |
| ------------- | ------------------------ | ----------------------------- | ---------------------------------------- |
| CPU interface | MMIO                     | System registers (fast)       | MMIO                                     |
| IPI mechanism | MMIO write to GICD_SGIR  | Sysreg write to ICC_SGI1R_EL1 | Dedicated register, delivered as FIQ     |
| IRQ routing   | 8-bit CPU mask           | 32-bit affinity (IROUTER)     | Hardware decides (not software-routable) |
| Max cores     | 8                        | Thousands                     | ~dozens                                  |
| MSI/LPI       | No                       | Yes (PCIe)                    | Own mechanism                            |
| Per-core init | CPU interface MMIO       | Redistributor + sysregs       | Single init                              |
| Shipped on    | Nothing modern           | Every non-Apple ARM SoC       | Apple Silicon                            |

Apple AIC specifics:

- Centralized controller, one MMIO register block (not per-core distributed)
- IRQs delivered to a single core chosen by hardware
- IPIs use dedicated registers, delivered as FIQ (faster entry than IRQ). Asahi Linux found this slightly faster than GIC SGIs.
- Timer interrupts also delivered as FIQ on M1
- The `InterruptController` trait gets an `AppleAic` implementation when M1 work begins

MSI/LPI note: GICv3's LPI (Locality-specific Peripheral Interrupt) support is needed for PCIe devices. This is a separate concern from the core trait — it would be a trait extension or separate trait when PCIe support is added (storage, USB, networking on real hardware all come through PCIe).

### Connection to M1 bare metal (broader)

Full M1 support requires replacing every driver (AIC, UART, SPI keyboard, ANS storage, DCP/AGX display). The interrupt controller trait is the first piece and has standalone value (cleaner kernel code, testability). See session discussion 2026-03-16 for the full M1 gap analysis:

- **MVP (boots, shows pixels, takes input):** 2-4 months. m1n1 framebuffer, AIC, UART, USB/SPI keyboard.
- **Feature parity with QEMU demo:** 4-8 months. Add ANS storage, proper display, reliable input.
- **Usable (networking, power, Thunderbolt):** 1+ year.

The kernel core (scheduler, syscalls, memory management, IPC), all of userspace, and the entire rendering pipeline are unchanged — they sit above the driver layer.

---

## Rendering Architecture: Path-Centric Pipeline (2026-03-16)

**Status:** COMPLETE. All three implementation phases shipped (2026-03-16). Design settled, implemented, and verified.

### Context

Systematic top-down audit of the rendering stack revealed architectural problems that can't be fixed incrementally. The compositor (~4,800 lines) is not the "content-agnostic pixel pump" the architecture document describes — it has font knowledge, SVG parsing, content-type dispatch, and glyph rasterization. Two incompatible rendering visions (path-centric and content-type-dispatching) coexist. Responsibilities leak across boundaries. Optimization complexity (change lists, PREV_BOUNDS, copy-forward) compensates for missing structural simplicity.

Researched Vello (Google/Linebender) — Raph Levien's GPU-compute-centric 2D renderer. Key findings: (1) Vello's Scene API matches the path-centric interface proposed here. (2) Vello now has three backends — GPU compute (production), CPU "sparse strips" (alpha), and hybrid CPU/GPU (experimental). (3) Raph's March 2025 blog post reveals frustration with GPU bounded-memory limitations. (4) Vello's implementation assumes host-OS infrastructure (wgpu, multithreading, std) incompatible with bare metal. (5) The architectural _principles_ and _API shape_ are proven and adoptable without the implementation.

### Decision: Path-Centric Rendering

**The rendering pipeline is a series of data shape transformations. Each component is a translator — data of one shape goes in, data of another shape goes out. The logic is fully encapsulated.**

```text
Hardware Events → Input Driver → Key Events → Editor → Write Requests
→ Core → Scene Tree → Render Backend → Pixel Buffer → GPU Driver → Display
```

Five data shapes (interfaces):

1. **Hardware Events** — evdev interrupts (type, code, value)
2. **Key Events** — logical key + modifier set
3. **Write Requests** — insert(pos, data), delete(pos, len), move_cursor(pos), set_selection(start, end)
4. **Scene Tree** — tree of containers, rects, glyphs, images in logical coordinates
5. **Pixel Buffer** — BGRA8888 framebuffer

Four translators (black boxes):

1. **Input Driver** — Hardware Events → Key Events
2. **Editor** — Key Events → Write Requests (with read-only document access)
3. **Core** — Write Requests → Scene Tree (owns document state, text shaping, layout)
4. **Render Backend** — Scene Tree → Pixel Buffer (owns tree walk, rasterization, compositing)

See `design/rendering-pipeline.mermaid` for the visual diagram.

### Sub-Decisions

**1. Glyph rasterization lives in the render backend, not core.**
Core does shaping (harfrust) and layout (line breaking, wrapping, cursor positioning). Core knows what text says and where it goes. The render backend knows what text looks like — it rasterizes glyph outlines, manages the glyph cache, handles subpixel rendering. Core submits glyph IDs + positions + font reference. This preserves the logical-coordinate model (core doesn't know the scale factor) and allows a future GPU backend to do SDF or outline rendering directly.

**2. Scene tree with geometric content types.**
The tree structure is retained (not a flat command stream) because: damage tracking needs stable node IDs, the tree maps naturally to document structure, and bare-metal CPU rendering isn't fast enough to repaint every pixel every frame at Retina resolution. The tree walk moves from the compositor into the render backend — push complexity to the leaf.

Node types:

- **Container** — geometry (x, y, w, h), decoration (background, border, corner_radius, opacity), flags (clips_children, visible), children (first_child, next_sibling). Like SVG `<g>`.
- **FillRect** — positioned rectangle with color. Optimization of a rectangular path.
- **Glyphs** — font reference + array of (glyph_id, x, y) + paint. "Cached vector shapes looked up by ID." Text is the common case, but monochrome icons are the same — an icon set is structurally identical to a font. This eliminates the 795-line SVG parser in the current compositor.
- **Image** — pixel data reference + bounds. Escape hatch for inherently raster content (photos, video frames).

The compositor doesn't dispatch on content type to _interpret_ data — it calls `backend.render(scene, surface)` and the backend handles everything.

**3. Explicit `RenderBackend` trait.**

```rust
trait RenderBackend {
    fn render(&mut self, scene: &SceneReader, target: &mut Surface);
    fn dirty_rects(&self) -> &[DirtyRect];
}
```

One call — the backend owns the tree walk, transform/clip stack, glyph cache, rasterization, compositing, and damage tracking. The compositor becomes ~30 lines: event loop, scene read, `backend.render()`, present.

**4. Multi-core rasterization is internal to the render backend.**
The kernel supports 4 SMP cores. The render backend can divide the framebuffer into horizontal strips and rasterize in parallel. This is an implementation detail of the leaf node — no interface changes, nothing above it knows or cares.

### What Changes From Today

| Responsibility              | Current location                             | New location                                   |
| --------------------------- | -------------------------------------------- | ---------------------------------------------- |
| Tree walk + compositing     | Compositor (scene_render.rs, 1807 lines)     | Render backend                                 |
| Glyph rasterization + cache | Compositor (scene_render.rs)                 | Render backend                                 |
| SVG parsing                 | Compositor (svg.rs, 795 lines)               | Eliminated — icons become Glyphs               |
| Damage tracking             | Compositor (damage.rs) + Core (change lists) | Render backend (internal)                      |
| Transform/clip stack        | Compositor (scene_render.rs)                 | Render backend                                 |
| Font metrics for layout     | Core (typography.rs)                         | Core (unchanged)                               |
| Text shaping                | Core (harfrust)                              | Core (unchanged)                               |
| Scene graph content types   | Text, Image, Path (semantic)                 | Container, FillRect, Glyphs, Image (geometric) |

### What the Compositor Becomes

```rust
fn main() {
    let backend = CpuBackend::new(scale_factor, fonts);
    let scene = DoubleReader::from_buf(scene_shm);
    let mut fb = Surface::from_buf(fb_shm, w, h, stride, format);
    loop {
        wait(&[core_handle, timer_handle]);
        if frame_scheduler.should_render() {
            backend.render(&scene.read(), &mut fb);
            send_present(gpu_handle, backend.dirty_rects());
        }
    }
}
```

### Connection to Vello

Adopting Vello's **architectural principles** and **API shape**, not its implementation. Vello assumes host-OS infrastructure (wgpu, multithreading, std) that doesn't exist on bare metal. What transfers:

- Vello's Scene API design validates the path-centric interface
- Vello's `vello_cpu` sparse strips algorithm is worth studying for the CPU backend
- Vello's three-backend architecture validates the explicit trait approach
- Existing code (NEON SIMD, scanline rasterizer, gamma-correct blending, glyph cache) becomes the CPU backend implementation

### Research Findings: Vello (Raph Levien / Google Linebender)

**Architecture:** GPU-compute-centric. CPU uploads scene in binary SVG-like format, compute shader pipeline handles everything: path flattening → binning → coarse rasterization → fine rasterization. Sort-middle architecture — paths stay sorted for compositing order, segments within paths are unsorted (winding number is commutative).

**Scene API:** `fill(shape, brush)`, `stroke(shape, brush, style)`, `push_layer(blend, clip)` / `pop_layer()`, `draw_image(image, transform)`, `draw_glyphs(font, glyphs)`. No specialized primitives — rectangles are rectangular paths, everything goes through the same pipeline.

**Three backends (as of late 2025):** GPU (production, requires WebGPU compute), CPU "sparse strips" (alpha, SIMD-oriented), Hybrid (experimental, CPU path processing + GPU fine rasterization for WebGL2).

**Limitations relevant to us:** Unbounded memory usage (intermediate buffers depend on scene complexity in unpredictable ways). Raph is frustrated with the GPU execution model's inability to use bounded queues between stages. GPU backend requires WebGPU — not available on bare-metal aarch64 with virtio-gpu 2D. CPU backend is alpha and designed for multithreaded host CPUs with wide SIMD.

**Performance:** GPU rendering is 10-100x faster than CPU for complex scenes. Intel HD 630 renders dense vector text at 7.6ms (60fps viable). The performance gap is the virtio-gpu VM boundary (guest→host copy), not our architecture — real hardware with DMA scanout eliminates this.

### Open Questions (deferred, don't block implementation)

1. Hinting — unnecessary at 2x Retina (~220 PPI). Additive later, no architectural impact.
2. Subpixel positioning (fractional x-advances for proportional text) — additive, render backend concern.
3. Complex script support (Arabic, Devanagari, CJK) — harfrust handles shaping, render backend needs compound glyph support.
4. Color emoji — would use Image content type, or OpenType COLR/CPAL layered glyphs (same pipeline).
5. Wide gamut color (Display P3) — render backend concern, additive.

### Implementation Plan — Three Phases

Each phase is independently shippable and testable. No big-bang rewrite.

**Phase 1: Extract** — Create render backend as wrapper around existing code. _(Mission-scale)_ ✅ Complete (2026-03-16)

Goal: Decouple the compositor from rendering without changing any behavior. Pure extract-and-encapsulate refactor.

- [x] Create `libraries/render/` with `trait RenderBackend { fn render(&mut self, scene: &SceneReader, target: &mut Surface); fn dirty_rects(&self) -> ...; }`
- [x] Implement `CpuBackend` by **moving** tree walk, compositing, glyph rasterization, damage tracking, and transform/clip stack code from compositor into it
- [x] Compositor calls `backend.render()` instead of doing rendering inline
- [x] Scene graph format UNCHANGED. Content types UNCHANGED. Behavior UNCHANGED.
- [x] All existing tests pass identically. QEMU visual verification unchanged.

What moved: `scene_render.rs` (~1807 lines), `damage.rs` (~81 lines), `compositing.rs` (~242 lines), `cursor.rs` (~85 lines), `svg.rs` (~795 lines), glyph cache setup. What stayed in compositor: event loop (~30 lines), frame scheduler, config handling, present signaling.

**Phase 2: Redesign** — Change the scene graph interface to geometric content types. _(Mission-scale)_ ✅ Complete (2026-03-16)

Goal: Replace semantic content types with geometric ones. This is the real architectural change.

- [x] Change scene `Content` from `{Text, Image, Path}` to `{FillRect, Glyphs, Image}`
- [x] Update `CpuBackend` to handle the new content types
- [x] Update Core's scene builder to produce the new content types
- [x] Update scene library: node structure, writer/reader APIs
- [x] Update all scene tests. QEMU visual verification unchanged.

Phase 1 meant only three things changed in coordination: scene library (interface), core (producer), render backend (consumer). The compositor was already decoupled. SVG parser and all path rendering code eliminated atomically. Core emits one `Glyphs` node per visible text line, `FillRect` for cursor and selection. Icons use the glyph cache.

**Phase 3: Clean up** — Eliminate dead code and boundary violations. ✅ Complete (2026-03-16)

Goal: Harvest the architectural benefits. Remove everything that doesn't belong.

- [x] Remove SVG parser (icons become Glyphs via icon font) — eliminated in Phase 2
- [x] Verify layout helpers in core, not scene library — `layout_mono_lines`, `byte_to_line_col`, `scroll_runs` confirmed in `core/scene_state.rs`
- [x] Minimize compositor — 174 lines, zero font knowledge, zero content dispatch, no SVG
- [x] Consolidate font handling: core owns shaping + metrics, render backend owns rasterization + glyph caching
- [x] Final test pass, QEMU visual verification — all tests pass, visual output identical

**All three phases complete.** The rendering pipeline now achieves the settled architecture: Core produces geometric scene trees, the render backend consumes them, and no component above the render backend has content-type knowledge.

---

## Rendering Pipeline Optimization (2026-03-15)

**Status:** Complete. Three milestones shipped and verified. 93 new tests (1462 → 1555). QEMU visual verification passes.

### Context

The rendering pipeline rebuilt the entire scene graph and repainted the full framebuffer on every event — keystroke, cursor blink, clock tick. At high resolution this produced visible lag. The optimization was delivered in three incremental milestones, each independently shippable and testable, following the design breadboarded in the "Resolution-Independent Rendering + Dirty-Rect Optimization" journal entry.

### Milestone 1 — Incremental Scene Graph Updates (OS service)

**Problem:** Core rebuilt the entire scene graph (~500 nodes, 64 KiB data buffer) on every event, even when only a clock digit or cursor position changed.

**Solution:** Copy-forward pattern + targeted update dispatch.

- `SceneHeader` extended with a **change list**: 24-entry array of changed `NodeId`s plus a `FULL_REPAINT` sentinel (0xFFFF) for overflow.
- `DoubleWriter::copy_front_to_back()` copies the current front buffer to back before mutation, enabling incremental edits instead of full rebuilds.
- `SceneWriter::mark_changed(node_id)` records which nodes were modified for the compositor to read.
- `SceneState` gained targeted update methods: `update_clock` (0 allocations — modifies clock text node in-place), `update_cursor` (0 allocations — repositions cursor node), `update_document_content` (rebuilds text runs after edits), `update_selection` (updates selection overlay).
- Core's event loop classifies each event and dispatches to the narrowest method. Timer ticks → `update_clock`. Cursor blink → `update_cursor`. Keypresses → `update_document_content`.
- **Data buffer exhaustion fallback:** when data buffer exceeds 75% capacity, triggers a full rebuild to compact references.

### Milestone 2 — Compositor Damage Tracking

**Problem:** Compositor repainted the entire framebuffer from the scene graph on every frame, even when only one node changed.

**Solution:** Change-list-driven damage + subtree clip skipping.

- Compositor reads the change list from the scene header instead of diffing the full scene.
- **PREV_BOUNDS tracking:** stores each node's previous bounding rect. Damage includes both old and new positions, preventing ghost artifacts when nodes move (e.g., cursor repositioning leaves no trail).
- `render_node` checks each child's bounds against the clip rect before recursing — children outside the clip region are skipped entirely.
- **Empty change list → skip rendering entirely.** No wasted work when nothing changed.
- Removed the old `diff_scenes` byte comparison from the render loop (replaced by change list).

### Milestone 3 — Pixel-Level SIMD and Unsafe Optimization (Drawing Library)

**Problem:** Per-pixel blending operations were the remaining bottleneck for damage-clipped rendering of large regions.

**Solution:** Three tiers of optimization, all behind unchanged public interfaces.

1. **Scalar optimizations:** `x / 255` replaced with `(x + 1 + (x >> 8)) >> 8` (exact for all u16 inputs). Pre-clipped iteration ranges computed up front in `draw_coverage`, `blit_blend`, `fill_rect_blend` — handles negative coordinates and large offsets without per-pixel bounds checks.
2. **Unsafe inner loops:** `draw_coverage`, `blit_blend`, `fill_rect_blend` use raw pointer access after row-level bounds verification. Eliminates redundant bounds checks.
3. **NEON SIMD (aarch64):** `fill_rect` writes 4 pixels per instruction via `vst1q_u32`. Alpha blending uses scalar sRGB gamma table lookups combined with NEON vector operations for linear-space blend math. Constant-color blends use a dedicated NEON fast path. All SIMD paths have scalar fallbacks and are validated against reference implementations.

### Results

- **93 new tests** across all three milestones (1462 → 1555 total). All pass.
- **QEMU integration test** passes (19/19 visual checks).
- **Pressure points §5.1 (NEON blending) and §5.2 (damage tracking)** in DESIGN.md resolved.
- The public interfaces (`blit_blend`, `fill_rect`, `draw_coverage`, `SceneWriter`, `SceneReader`) are unchanged — all optimization is internal to leaf nodes, as the design predicted.

### Files Changed

- `libraries/scene/lib.rs` — change list in header, `copy_front_to_back`, `mark_changed`, targeted update types
- `services/core/scene_state.rs` — `update_clock`, `update_cursor`, `update_document_content`, `update_selection`, event dispatch
- `services/core/main.rs` — event loop wired to targeted dispatch
- `services/compositor/damage.rs` — change-list reader, PREV_BOUNDS tracking
- `services/compositor/scene_render.rs` — subtree clip skipping, damage-clipped rendering
- `libraries/drawing/lib.rs` — div255, pre-clipped iteration, unsafe inner loops, NEON SIMD paths
- `test/tests/` — 93 new tests across scene, compositor, and drawing test files

---

## Input Handling Architecture (2026-03-15)

**Status:** Complete. Core architecture implemented and validated. The three-layer input pipeline (physical → logical → semantic) is proven end-to-end: core tracks modifier state and delivers logical key + modifiers via IPC, editors resolve characters and compute cursor positions, cursor/selection are OS primitives. Demonstrated through text editor with shift+arrow selection, mouse click-to-position, and Ctrl+Tab system shortcuts. Remaining keybindings (Cmd+arrows, Option+word-boundaries, clipboard) are incremental implementation work on settled patterns.

### Context

Text input support is minimal — only lowercase characters, no modifier handling. Designing "full" keyboard input (shift, capslock, Cmd+arrow navigation, selection, etc.) to match the macOS editing experience. This required settling where each layer of input interpretation lives in the system.

### Design Decisions

**1. Three-layer input pipeline: physical → logical → semantic.**

- **OS: physical keycode → logical key + modifiers.** The OS tracks modifier state (Shift, Ctrl, Option, Cmd held; Caps Lock toggled) from raw evdev key-down/key-up events. It resolves physical keycodes through the active keyboard layout to produce logical keys. Output is always (logical key, modifier set) — never characters.
- **OS → Editor: logical key + modifiers via IPC.** The OS delivers the same event format to every editor regardless of content type. The OS never interprets what a key combination means semantically.
- **Editor: key + modifiers → characters and/or operations on OS primitives.** The editor decides what keys mean. A text editor resolves (key=A, modifiers=Shift) to character 'A' and calls write-insert. It resolves (key=Left, modifiers=Cmd) to "start of line" and calls move-cursor-to. An image editor interprets the same keys differently.

**2. Character resolution lives in the editor, not the OS.**

The OS does NOT resolve keys to characters. The editor is the input method. This means:

- Switching from English to Japanese input is switching editors, not changing a system setting. The Japanese editor handles romaji → hiragana → kanji conversion internally.
- Different editors can produce different characters for the same key combinations (virtual keyboards, language-specific editors).
- The OS stays simpler — it only needs keyboard layout knowledge (physical → logical key) for its own system shortcuts.

**3. Cursor and selection remain OS primitives (reinforced).**

Cursor position and selection state are owned by the OS, not by editors. This was questioned during the discussion but holds for two reasons:

- **View mode:** Cursor and selection exist without any editor active (text selection for copy, playhead in video). No editor means cursor can't be editor state.
- **Editor swap continuity:** When switching from an English editor to a Japanese editor on the same document, the cursor position survives because it's OS state. The new editor picks up exactly where the old one left off.

The editor computes cursor movement (it knows what "start of line" means at byte offset 247) and tells the OS where to put the cursor. The OS stores the position and renders it.

**4. System-wide shortcuts are intercepted by the OS before reaching editors.**

Cmd+Q, Cmd+Tab, and other system gestures are handled at the OS level. The OS needs keyboard layout resolution for this (it must know that a physical key + Cmd is "Q"), which it already has from layer 1. Editors never see system shortcuts.

**5. System UI text fields use the editor framework.**

The command bar, search fields, and dialogs need character input but aren't documents with editors. Rather than duplicating character resolution in the OS, these use the same editor framework — system text fields are mini-documents with an editor attached. Consistent architecture, even if it's more work initially.

**6. Coarse undo via COW snapshots is sufficient.**

Cmd+Z triggers OS-level undo (restore previous COW snapshot at operation boundary). No per-character undo. Editors define operation granularity via beginOperation/endOperation. This works regardless of which editor is active — no editor needs to implement undo logic.

**7. macOS Emacs bindings (Ctrl+A/E/K/D/F/B/N/P) are excluded.**

These legacy terminal shortcuts add complexity for no value. Cmd+arrows cover the same navigation.

### Interface Summary

```text
OS → Editor (IPC):
  key_event(logical_key, modifiers)    // always this, never characters

Editor → OS (IPC):
  move_cursor_to(position)             // navigation
  set_selection(start, end)            // selection
  write_insert(data)                   // character/text input
  write_delete(range)                  // backspace, forward delete, etc.
```

### Target Keybindings (text editor, first implementation)

**Navigation (editor computes position, calls move_cursor_to):**

- Arrow keys: character left/right, line up/down
- Cmd+Left/Right: line start/end
- Cmd+Up/Down: document start/end
- Option+Left/Right: word boundaries
- Page Up/Down (when scrolling views exist)

**Selection (editor computes range, calls set_selection):**

- Shift + any navigation key
- Cmd+A: select all

**Editing (editor resolves characters, calls write_insert/write_delete):**

- Shift/Caps Lock for uppercase + symbols
- Backspace: delete backward
- Delete (fn+Backspace): delete forward
- Option+Backspace: delete word backward
- Option+Delete: delete word forward
- Cmd+Backspace: delete to line start
- Enter, Tab

**Clipboard (OS-level):**

- Cmd+C/X/V: copy/cut/paste
- Cmd+Z: undo (COW snapshot restore)

### Design Implications

- **Word boundary detection** requires the editor to understand word segmentation for the content it's editing. For text, this means Unicode word boundaries. Since the editor already does text layout (settled 2026-03-13), it has the text content and can compute word boundaries.
- **The "editor is the input method" model** means content-type registration metadata should declare the editor's input capabilities (what languages/scripts it supports), enabling the OS to present appropriate editor choices.
- **Emoji input** is a future concern — likely a system-level picker that inserts via the active editor's write_insert path.

---

## Boot Display via ramfb (2026-03-15)

**Status:** Design validated, implementation deferred. See findings below.

### Context

Currently the OS outputs diagnostic text to the host terminal (serial) and renders the UI in the QEMU window (virtio-gpu). The QEMU window is blank during boot because virtio-gpu requires userspace driver initialization. On real ARM hardware with UEFI, firmware provides an immediate framebuffer (EFI GOP) that the kernel can write to from its first instruction — boot text on the physical display is the natural default.

### Design Decisions (Validated)

**1. Firmware framebuffer for early boot display.**
The kernel writes to a pre-GPU framebuffer early in boot, then the GPU driver takes over. No virtio negotiation required. This mirrors the real-hardware pattern of firmware framebuffer → GPU driver handoff.

**2. Serial and boot display are independent, simultaneous channels.**
They serve different audiences. Both are active during boot; neither suppresses the other.

- **Serial (host terminal):** Full structured diagnostic log. Everything verbose — memory map, page table setup, SMP core bringup, interrupt controller init, subsystem lifecycle, timing data, warnings/errors with full context. The developer/operator channel. Survives display driver crashes.
- **Boot display (physical display):** Curated user-facing narrative. A small number of milestone messages conveying boot progress at a human-meaningful level. Not implementation details.

**3. Curated milestone messages (not wall-of-text, not just a logo).**
Each message corresponds to a phase where, if it hangs, the user knows _where_ it hung. That's the practical utility beyond aesthetics. Approximate milestones:

1. "Starting up" — kernel entry, MMU, memory
2. "Starting processors" — SMP bringup (perceptible latency on real multi-core hardware)
3. "Connecting devices" — virtio probing, storage, input (can stall on missing/slow hardware)
4. "Loading fonts" — font file I/O; signals "about to show you something"
5. "Preparing your workspace" — OS service, compositor init, document pipeline ready
6. UI appears (compositor takes over via virtio-gpu)

**4. Growing list with visual dimming.**
Messages accumulate as a list. The current (most recent) message renders bright; previous messages dim to grey. This provides:

- Forward-motion feel without explicit animation — contrast shift as each new line appears
- "Where did it stop" diagnostic value — the full list is visible if boot hangs
- Visual weight stays on the current phase, not the history

**5. Hard cut to compositor.**
When the GPU driver takes over, the boot display content is simply replaced. No fade, no transition. This matches what real hardware does (GPU driver takes over the display). Revisit later only if it feels wrong in practice.

### Implementation Spike — ramfb on QEMU (2026-03-15)

Fully implemented and working: fw_cfg MMIO driver, DMA-based ramfb initialization, 8x16 bitmap font, milestone display with dimming. Three milestones visible on screen, correct visual hierarchy. **Then discarded** — the QEMU display model makes this unusable in practice.

#### What worked

- **fw_cfg MMIO protocol:** Selector register (16-bit BE write at base+0x08), data register (byte reads at base+0x00), DMA register (two 32-bit BE writes at base+0x10). Signature verification ("QEMU"), feature bitmap check (DMA bit), file directory enumeration to find "etc/ramfb".
- **DMA descriptor construction:** 16-byte descriptor (control u32 BE, length u32 BE, address u64 BE) + 28-byte RAMFBCfg payload (addr u64 BE, fourcc u32 BE, flags u32 BE, width u32 BE, height u32 BE, stride u32 BE). Must be in a stable allocated page — stack-based descriptors fail. Trigger via two 32-bit writes to DMA register; each write independently `.to_be()`.
- **Bitmap font:** 8x16 VGA font (95 printable ASCII glyphs, 1520 bytes). `draw_char`/`draw_string` writing BGRA8888 directly to framebuffer memory.
- **Milestone display:** IrqMutex-protected state, redraw on each milestone with bright current / grey previous.

#### What broke (and fixes)

1. **Data abort (EC=0x25):** fw_cfg selector register requires exactly 2-byte access. QEMU enforces `max_access_size=2`; a 4-byte write triggers a bus abort. Fix: `write16` MMIO helper.
2. **DMA error (ctl=0x01):** Stack-based DMA descriptor instability. Fix: allocate dedicated 4KB page, build descriptor with `copy_nonoverlapping` at known offsets.
3. **DMA timeout:** Endianness on DMA trigger — each of the two 32-bit writes needs independent `.to_be()`, not a single `.to_be()` on the 64-bit value.

#### Why it was discarded

**QEMU's `-device ramfb` and `-device virtio-gpu-device` register as two independent display consoles.** ramfb is console 0, virtio-gpu is console 1. QEMU's display window shows console 0 by default. When virtio-gpu calls `set_scanout` and flushes its first frame, it updates console 1 — but the user still sees console 0 (ramfb) unless they manually switch (Ctrl+Alt+2 in GTK).

There is no guest-accessible mechanism to disable ramfb's console or switch the active display from inside the kernel. A combined `virtio-ramfb` device exists in QEMU (shares one console, handles the handoff internally) but it's PCI-only — not available on the `virt` machine with virtio-mmio transport.

**On real hardware this problem doesn't exist.** There's one physical display. The firmware framebuffer (EFI GOP, simplefb, etc.) occupies it. When the GPU driver does modesetting, the GPU's output replaces the firmware framebuffer on the same physical display. One output, one screen — the handoff is automatic.

#### Decision

Defer boot display until either: (a) targeting real hardware with EFI GOP / simplefb, or (b) QEMU adds `virtio-ramfb` support for the `virt` machine / virtio-mmio transport. The design is validated, the protocol details are documented above, and the implementation was proven to work. ramfb is a QEMU-only device that doesn't work well on QEMU — not worth the UX cost of requiring manual console switching.

Waiting for virtio-gpu to be ready before showing boot milestones defeats the purpose — by the time the GPU pipeline is live, boot is already over.

---

## Resolution-Independent Rendering + Dirty-Rect Optimization (2026-03-15)

**Status:** Pieces 1-2 shipped (see "Rendering Pipeline Optimization" entry). Piece 3 (logical coordinate model) remains open.

### Problem

The UI is noticeably laggy at host-native resolution (3456×2234 on Retina). Root cause: the compositor repaints the entire 31 MB framebuffer and the GPU driver transfers the entire 31 MB through virtio MMIO on every frame — even when only a cursor blink or single keystroke changed. The dirty rect infrastructure already exists end-to-end (DirtyRect type, DamageTracker, PresentPayload with 6 rects, GPU driver partial TRANSFER_TO_HOST_2D) but the compositor always sends `rect_count: 0` (full-screen transfer).

Additionally, the scene graph uses physical pixel coordinates. The OS service receives `fb_width`/`fb_height` from init and lays out directly to those values. There is no logical/physical coordinate distinction — layout is resolution-specific.

### Design Discussion

**Scene graph coordinate model — logical vs physical:**

Two options: (A) scene graph in logical coordinates (points), compositor applies scale factor; or (B) scene graph in physical pixels, OS service pre-applies scale.

Decision: **Logical coordinates (option A).** Rationale:

- Isolate uncertain decisions behind interfaces (founder claim). The OS service shouldn't know or care about the display's physical resolution. It thinks in points.
- The scene graph is rebuilt on every state change anyway (keystroke, scroll, cursor move), so "resolution change requires a scene rebuild" isn't an extra cost.
- Clean separation: OS service declares _what_ to render (in its coordinate space), compositor decides _how_ (at the physical resolution it knows).
- Font rendering: compositor rasterizes glyphs at physical pixel size (`logical_font_size × scale_factor`). Optical sizing uses physical DPI. Both are compositor concerns.
- Integer precision at i16 logical coordinates with 2× scale: every logical pixel maps to 2 physical pixels. Sub-pixel glyph positioning is a compositor concern (during rasterization), not a scene graph concern. macOS uses the same model.
- Pixel-snapping for borders/dividers: compositor rounds to nearest physical pixel. Not purely multiply-by-scale — it makes snapping decisions too.

The scale factor becomes compositor config (alongside DPI), received from init. The OS service never sees it.

**Dirty-rect optimization — scene diffing vs damage declaration:**

Two approaches: (A) compositor diffs old vs new scene graph, derives dirty rects automatically; or (B) OS service declares damage regions explicitly (Wayland model).

Decision: **Scene diffing (option A).** Rationale:

- The scene graph is a flat array of fixed-size repr(C) nodes — diffing is `memcmp` per slot. Cheap.
- As the OS service grows in complexity (compound documents, multiple editors, layout engine), requiring it to perfectly track damage is a maintenance burden that compounds. Diffing absorbs that — the OS service just rebuilds the scene graph however it wants, and the compositor figures out what changed.
- Same reasoning as React: developers stopped manually tracking DOM mutations and let the framework diff. The expensive operation is pixel rendering, not scene construction.
- The compositor already keeps the scene in shared memory. Double-buffering the scene (keeping the previous version) adds one memcpy of the node array per frame — negligible vs the rendering cost.

### Incremental Delivery

Pieces chain together, each independently shippable and testable:

**Piece 1 — Activate existing dirty-rect GPU transfer.**
The compositor already repaints everything. Add a scene diff step: compositor keeps a copy of the previous scene graph, compares per-node, computes bounding rects of changed nodes, passes dirty rects in PresentPayload. The GPU driver already handles `rect_count > 0`. Highest leverage (cuts 31 MB MMIO transfer to a few KB for a keystroke), lowest risk (no interface changes, no scene graph format changes).

But: the compositor also repaints the entire framebuffer from the scene graph. Even with partial GPU transfer, every frame still touches every pixel in the back buffer. Piece 2 addresses this.

**Piece 2 — Dirty-rect clipped rendering.**
Compositor clips its rendering to the dirty region. `render_scene` currently takes a full-framebuffer clip rect — narrow it to the dirty rect union. Nodes outside the dirty region are skipped. Overlapping nodes within the dirty region are repainted (back-to-front within the rect). Requires copying the previous framebuffer's clean regions to the back buffer (or using the same buffer with dirty-rect writes only).

**Piece 3 — Logical coordinate model.**
Change scene graph Node.x/y/width/height to logical coordinates. Add scale_factor to compositor config. Compositor multiplies all positions and sizes by scale_factor during rendering. OS service lays out in points. Font sizes in scene graph are logical; compositor computes `physical_px = logical_size × scale_factor` for rasterization. This is the interface/structural change.

### Breadboard — Piece 1: Scene Diff + Partial GPU Transfer

```text
compositor render loop (current):
  wait for scene update
  render_scene(back_fb, scene_graph)         ← repaints everything
  send MSG_PRESENT(buffer_index, rect_count=0) ← full transfer
  swap buffers

compositor render loop (piece 1):
  wait for scene update
  diff(prev_scene_nodes, curr_scene_nodes)   ← NEW: per-node memcmp
    → collect changed node indices
    → compute bounding rects of changed nodes (in abs coords)
    → union into dirty region (up to 6 DirtyRects)
  render_scene(back_fb, scene_graph)         ← still repaints everything (piece 2 fixes this)
  send MSG_PRESENT(buffer_index, rect_count=N, rects=[...])  ← partial transfer
  memcpy curr_scene_nodes → prev_scene_nodes ← save for next diff
  swap buffers
```

**Scene diff algorithm:**

```rust
fn diff_scenes(
    prev: &[Node; MAX_NODES],
    curr: &[Node; MAX_NODES],
    node_count: usize,
) -> DamageTracker {
    let mut damage = DamageTracker::new(fb_width, fb_height);
    for i in 0..node_count {
        // Fixed-size repr(C) — byte comparison is correct and fast.
        let prev_bytes = &prev[i] as *const Node as *const [u8; NODE_SIZE];
        let curr_bytes = &curr[i] as *const Node as *const [u8; NODE_SIZE];
        if prev_bytes != curr_bytes {
            // Node changed. Compute its absolute bounding rect
            // by walking parent chain (or: store abs coords in prev pass).
            let rect = abs_bounds(curr, i);
            damage.add(rect.x, rect.y, rect.w, rect.h);
            // Also damage the OLD position if the node moved.
            let old_rect = abs_bounds(prev, i);
            damage.add(old_rect.x, old_rect.y, old_rect.w, old_rect.h);
        }
    }
    damage
}
```

**Pressure point — computing absolute bounds:** Nodes store positions relative to parent. To get the absolute bounding rect for a changed node, you need to walk up the parent chain. But the scene graph uses left-child/right-sibling (no parent pointer). Options:

1. Pre-compute absolute positions during the render walk and cache them.
2. Add a `parent: NodeId` field to Node (4 bytes, increases node size).
3. Walk the tree once to build a parent map (array of NodeId, indexed by node index).

Option 3 is cheapest — one pass over the node array building `parent[i]` from `first_child`/`next_sibling`, then `abs_bounds` walks up via `parent[]`. The parent map is the same size as the node array (512 × 2 bytes = 1 KB) and rebuilt each frame.

**Pressure point — nodes added/removed:** If the node count changes between frames, new nodes have no prev entry and removed nodes leave stale prev entries. Handle by: if `curr_node_count != prev_node_count`, dirty the entire screen (fall back to full repaint). This is rare (only on document open/close, not on typing). Alternatively, diff up to `min(prev_count, curr_count)` and dirty any nodes beyond that range.

**Pressure point — data buffer changes:** A node's text content can change without the node struct changing (if the text is referenced by DataRef with same offset/length but different bytes). This means struct-level memcmp can miss text edits. Fix: include a content hash in the node, or always diff data buffer contents for Text/Path nodes. The simpler approach: the OS service already writes new text at new data buffer offsets (append-only within a frame), so the DataRef offset will differ, which the memcmp catches.

### Implementation Status

All three pieces are implemented and running:

- **Piece 1 (scene diff + partial GPU transfer):** `diff_scenes()` in scene library, `DamageTracker` in compositor (`damage.rs`), change-list-driven damage in render loop, partial `MSG_PRESENT` with dirty rects. Uses `content_hash` (FNV-1a) in nodes to catch text edits where DataRef metadata is identical.
- **Piece 2 (dirty-rect clipped rendering):** Compositor renders only within dirty rects when `damage.dirty_rects()` returns `Some`. Full-screen fallback when node count changes between frames.
- **Piece 3 (logical coordinate model):** `scale_factor` flows from init → `CompositorConfig` → compositor. Scene graph is in logical coordinates; compositor multiplies by `scale_factor` during rendering. Font rasterization uses `physical_font_size = round(logical_size × scale_factor)`. Init auto-detects scale: ≥2048px wide → 2.0×, otherwise 1.0×.

### Remaining Open Questions

- Interaction between dirty-rect rendering and subpixel font rendering (dirty rect slicing through a glyph)
- Whether the DamageTracker's 6-rect limit is sufficient or needs a smarter merge strategy

---

## Scene Scroll Fix + Kernel TPIDR Race Fix (2026-03-14)

**Status:** Two bugs fixed. Both committed.

### Bug 1: Text overflow + cursor misalignment in scene builder

**Symptom:** Text runs extended past the viewport bottom (no scroll clipping). Cursor and selection rects positioned at absolute coordinates, misaligned when scrolled.

**Root cause:** `build_editor_scene` in core's `scene_state.rs` created text runs at absolute y positions (0, 20, 40...) without applying scroll. The compositor's `node.scroll_y` only offset children as a pixel value, but the runs were already at absolute positions. Cursor/selection used absolute `line * line_height` without subtracting scroll offset.

**Fix:** Extracted layout helpers from `scene_state.rs` to scene library as public functions: `layout_mono_lines`, `byte_to_line_col`, `line_bytes_for_run`, `scroll_runs`. Core now calls `scroll_runs(all_runs, scroll_lines, line_height, content_h)` to filter runs to the visible viewport and adjust y positions. Cursor y = `cursor_line * line_height - scroll_px` (viewport-relative). Selection rects clipped to viewport bounds. Compositor renders with `scroll_y = 0` and `CLIPS_CHILDREN` — core pre-applies all scrolling.

**Tests:** 11 new tests: monospace layout (basic, trailing newline, soft wrap, empty), byte-to-line-col (basic, soft wrap, cursor consistency with layout), scroll filtering (no scroll, filters above viewport, cursor at bottom, empty text). 943 total pass.

**Files:** `libraries/scene/lib.rs` (+125 lines), `services/core/scene_state.rs` (-19 net), `test/tests/scene.rs` (+140 lines).

### Bug 2: Kernel EC=0x21 instruction abort under SMP (TPIDR_EL1 race)

**Symptom:** Intermittent kernel crash — EC=0x21 (instruction abort at current EL) with ELR=FAR=0x0A003A00 (virtio MMIO physical address range, level 0 translation fault). Only manifested under concurrent load with 4 SMP cores and 5+ processes.

**Root cause:** `schedule_inner` parks the old thread in the ready queue and returns the new thread's context pointer. But `TPIDR_EL1` (used by `save_context` in exception.S to locate the write target) was only updated by exception.S _after_ the Rust handler returned — which is after the `IrqMutex` lock drops and re-enables IRQs. If a pending timer IRQ fires in that window (between lock release and the `msr tpidr_el1` in exception.S), `save_context` reads the stale TPIDR and overwrites the **old** thread's Context with kernel-mode state (SPSR=EL1h, ELR=kernel addr, SP from the wrong thread's stack). When the old thread is later scheduled from the ready queue, `eret` restores EL1 mode with a garbage low address as ELR, causing the instruction abort.

**Why the address was `0x0A003A00`:** This is a user-range VA (low address). With SPSR=EL1h, the eret returns to EL1 which uses TTBR0 for lower-half VA walks. If TTBR0 is the empty L0 table (kernel/idle thread) or any table without a mapping at that address, the walk fails at level 0 — exactly matching ESR=0x86000004 (IFSC=4, level 0 translation fault).

**Fix:** Set `TPIDR_EL1` to the new thread's Context pointer inside `schedule_inner`, while the scheduler lock is held and IRQs are masked. This ensures `save_context` always writes to the correct (current) thread's Context, even if an IRQ fires immediately after the lock drops. The `msr tpidr_el1` in exception.S is now redundant but kept as defense-in-depth. Also added `validate_context_before_eret` — checks ELR/SPSR/SP consistency before every eret return (catches EL1-to-user-VA, EL0-to-kernel-VA, and EL1-with-bad-SP).

This is the same class of bug as Fix 5 (aliasing UB) and Fix 6 (nomem on DAIF) — an SMP timing window that only manifests under concurrent scheduling pressure. The window was ~3-5 instructions wide (lock release → TPIDR write in asm), but with 4 cores running 250 Hz timers, it was hittable.

**Stress tested:** 3000 keys at 1ms intervals, 5 processes on 4 SMP cores. No crash. 943 tests pass.

**Files:** `kernel/scheduler.rs` (+21 lines), `kernel/main.rs` (+71 lines), `kernel/exception.S` (+6 lines comments).

---

## Compositor Split + Scene Graph Design (2026-03-13)

**Status:** Design conversation in progress. Key architectural decisions settling.

### Context

Reviewed the full userspace architecture above the kernel. The compositor (`services/compositor/`, 2260 lines) had accumulated two fundamentally different responsibilities: OS service work (document ownership, input routing, edit protocol) and rendering work (surface management, compositing, GPU presentation). These need to separate into distinct processes.

### Protocol Crate (completed)

Created `libraries/protocol/` as the single source of truth for all IPC message types and payload structs. 8 modules organized by protocol boundary (device, gpu, input, edit, compose, editor, present, fs). All 22 message type constants centralized; zero duplicates remain across the codebase. Net -333 lines.

Also fixed test infrastructure: libraries now have proper Cargo.toml files so the test crate uses normal Cargo dependencies instead of `#[path]` source includes. This eliminated a duplicate `DirtyRect` definition that existed solely to work around the test build limitation.

### Design Decisions Reached

**1. OS service and compositor are separate processes.**
Not a code-organization split — a process boundary with IPC. The OS service owns document semantics; the compositor owns pixels. This matches the design principle of simple connective tissue between components, and validates the IPC protocol at a real boundary.

**2. The interface between them is a scene graph.**
Evaluated three options:

- **Buffer-based (Wayland model):** OS service renders content into pixel buffers, compositor just composites. Simple but puts all rendering work in the OS service.
- **Scene-graph-based (Fuchsia Scenic, Core Animation, game engines):** OS service sends a tree of typed visual nodes, compositor renders and composites. More capable.
- **Command-based (X11, Plan 9 draw):** OS service sends drawing commands. Historical — every modern system moved away from this.

Chose scene graph because:

- "OS renders everything" means the OS _layer_ (everything in `/services` and `/libraries`), not specifically the OS service process. The compositor IS part of the OS.
- Layout and compositing are the same pipeline: document structure → positioned visual elements → pixels. The scene graph is the intermediate representation between those two stages.
- It naturally supports compound documents (Decision #14): the document's spatial/temporal/logical relationships compile to a scene tree.
- It doesn't artificially prevent game-engine-level rendering later (3D, animation, transforms) — you just add node types.

**3. The scene graph is NOT the document structure — it's a compiled output.**
The document model has semantic content (logical relationships, metadata, temporal sync) that the compositor doesn't need. The screen has visual elements (chrome, cursor, selection) that aren't in any document. The OS service _compiles_ document structure into a scene graph, like a compiler turns source into machine code.

**4. The scene graph lives in shared memory.**
Written by the OS service, read by the compositor. The compositor is a pure function from scene graph to pixels. No scene graph state inside the compositor — if it crashes, the graph is still there, just restart and re-render. Same pattern as the existing document buffer.

**5. The screen is the root compound document.**
The entire visual output can be thought of as a compound document with system chrome and document content as its parts. The compositor doesn't know it's rendering "the screen" — it renders a scene graph, and the screen is just the root node. No special case for "the desktop." Multi-document views are just different layout types for the root document.

### Research: Prior Art Surveyed

- **Fuchsia FIDL + Scenic:** Full IDL for IPC (FIDL), scene-graph compositor (Scenic). Validated the typed-channel approach and scene-graph-at-OS-level pattern.
- **Singularity OS contracts:** State-machine-typed channels. Too complex for single-language Rust OS, but the insight that channels should be typed per-protocol is applicable.
- **seL4 CAmkES:** Framework-generated typed interfaces. Userspace typing over untyped kernel transport — same pattern we're following.
- **Wayland:** Buffer-based compositor protocol. Good surface lifecycle model but doesn't match "OS renders everything."
- **Core Animation:** Property-based layer tree with animations. The hybrid model (pre-rendered backing stores in a scene tree) is close to the right answer.
- **Game engines (Unity, Godot, Bevy):** Scene tree of typed nodes with transforms and components. The compositor is essentially a document rendering engine — same structure.

### Additional Decisions (continued discussion)

**6. Interaction state (cursor, selection) belongs to the View, not to individual document nodes.**
The cursor navigates the compound document's flow layout, crossing content-type boundaries. The text editor owns "cursor within text" but the OS service owns "cursor within the compound document." When the cursor reaches the boundary of one content part (e.g., top of a paragraph), the OS service consults the layout to determine what's above/below and moves focus accordingly. Editors signal boundary-hit; the OS service navigates between content parts.

**7. Focus path model for compound document editing.**
The View tracks a focus path (stack) through the compound document tree. Each level has its own cursor state. "Zooming into" a sub-document pushes onto the stack; returning pops. Whether child cursor state is preserved on pop is a UX policy decision, not structural -- the architecture supports both.

**8. One node type with content variants (Core Animation model).**
Rejected separate types for Container, Text, Image, Path. Instead: one Node type with a rich visual base and an optional Content variant (None, Text, Image, Path). Inspired by CALayer.

Rationale:

- In compound documents, almost every container also needs visual properties (background, border, corner radius). Separate types force wrapper nodes everywhere.
- Fixed-size nodes in shared memory: one flat array, one allocation strategy, indices work uniformly.
- Core Animation proved this works at scale -- CALayer handles 95% of cases.
- Starting with four types, you'd gradually merge them anyway as each gains the others' properties.

Node carries: tree links (first_child, next_sibling as indices), geometry (x, y, width, height relative to parent), scroll_y, visual decoration (background, border, corner_radius, opacity), flags (clips_children, visible), and a content variant.

Content variants:

- None: pure container
- Text: string ref (offset+len into data buffer), font_size, color
- Image: pixel data ref (shm offset), source dimensions
- Path: command ref (offset+len into data buffer), fill, stroke

Variable-length data (text strings, path commands) lives in a separate data buffer region of shared memory, referenced by offset+length from the node.

**9. Relative positioning with scroll_offset solves scrolling immediately.**
Children are positioned relative to parent's content area. Scrolling = changing one scroll_y value on a clipping container. Compositor offsets all children during render. No OS service round-trip. Full declarative layout (flex, column, row) is the right end state but can be added later as a field on Node without structural change.

**10. The View is an OS service concept, not a scene graph concept.**
The View (focus path, cursor state, document binding) lives entirely in the OS service. The OS service translates View state into scene graph mutations (position cursor node, update text content, set scroll_y). The compositor never knows about Views, documents, or editing. It renders a tree of Nodes. Clean separation: scene graph = rendering interface, View = document interaction model.

**11. ~~Compositor owns text layout.~~ REVERSED (2026-03-14): OS service owns text layout.**
~~The compositor has the font rasterizer; it also owns line breaking, word wrapping, and glyph positioning.~~

**Revised (2026-03-14):** The OS service owns all text layout (line breaking, wrapping, glyph positioning, hit testing). The compositor is content-agnostic — it renders positioned visual elements without knowing what "text" is. This is consistent with the architecture: the OS service will house the layout engine for all content types (text, images, compound documents), so text is not a special case. The scene graph carries pre-laid-out content (positioned glyphs or pre-computed line breaks), not raw strings. The OS service needs font metrics (advance widths, line height — a few hundred bytes) but not the full rasterized glyph cache.

Prior art is unanimous: Core Animation, Wayland, Fuchsia Scenic, web browsers, game engines — all put text layout above the compositor, never inside it. Reason: text layout is content understanding, and the design axiom "OS natively understands content types" means that understanding belongs in the OS service, not the pixel pump.

Cursor and selection remain properties of the Text content variant for now. The OS service knows glyph positions (it did the layout) so it can position cursor and selection rects directly. The compositor renders them without understanding what they mean.

~~**TODO:** Redesign the Text content variant to carry positioned/pre-laid-out text instead of raw strings + width constraints.~~ **Done.** `TextRun` struct carries positioned runs with (x, y), `DataRef` to glyph data, `glyph_count`, `advance` width, `font_size`, and `axis_hash`. `Content::Text` holds a `DataRef` to an array of `TextRun`s plus `run_count`. `ShapedGlyph` struct supports per-glyph advances for proportional text (future). Cursor and selection are positioned pixel-coordinate rects (regular scene graph nodes with backgrounds). `push_text_runs()` / `text_runs()` / `front_text_runs()` APIs on `SceneWriter` / `SceneReader` / `DoubleReader`.

**TODO:** Better name for "View" (the thing that holds document + focus path + overlays).

### Scene Graph TODOs

- ~~**Ctrl+Tab image viewer**: Scene graph only builds text editor view. Need Image content node support or hybrid approach for image mode.~~ **Done.** `Content::Image` variant in scene graph with `DataRef` to pixel data and source dimensions.
- **Text editor keystrokes**: Up/Down arrows (line navigation), Cmd+arrow (start/end of line/document), Shift+key (uppercase), Delete (forward delete). ~100-150 lines in text-editor/main.rs. All mechanical — patterns established.

### Open Questions

1. **How do typed channels work?** `Channel<P>` API design, multiplexing across different protocol types with `wait()`.
2. ~~**Shared memory double-buffering.**~~ **Done (2026-03-14).** `DoubleWriter`/`DoubleReader` in scene library. Two `SCENE_SIZE` regions, generation-counter-based swap with release/acquire fences. Compositor `SceneState` migrated.
3. ~~**Scene graph shared memory layout.**~~ **Done (2026-03-14).** `SceneWriter`/`SceneReader` in scene library. Header (64 B) + node array (512 × Node) + data buffer (64 KiB). 34 host-side tests.
4. ~~**Text content variant redesign — settled on A' (positioned text runs).**~~ **Done.** `TextRun` struct with (x, y), glyph DataRef, advance width, font_size, axis_hash. `Content::Text` holds DataRef to TextRun array + run_count. `ShapedGlyph` for per-glyph advances (proportional future). Cursor/selection are positioned rects. Both core and compositor scene builders use the new representation.

### Implications for Existing Decisions

- **Decision #11 (Rendering Technology):** The "existing web engine" leaning may shift. If the compositor is a scene-graph renderer, a web engine becomes a content-type translator that produces scene graph nodes, not the rendering substrate. The scene graph IS the rendering engine.
- **Decision #15 (Layout Engine):** The layout engine compiles document structure into the scene graph. The "CSS for spatial" option still works — CSS layout produces positioned nodes that become scene graph nodes. But the layout engine is now clearly upstream of the compositor, not inside it.
- **Decision #14 (Compound Documents):** The three-axis relationship model (spatial, temporal, logical) maps to the scene graph. Spatial relationships become positions/transforms. Temporal and logical relationships may need scene graph support beyond static trees (animation timelines, visibility state).

---

## Kernel Bug Investigation Follow-up (2026-03-13)

**Status:** Resolved. Root cause of the EC=0x21 crash identified and fixed in 2026-03-14 session (Fix 17: TPIDR_EL1 race in `schedule_inner`). Fixes 12-16 below addressed contributing factors and separate bugs found during the same investigation.

**Context:** User reported kernel crash (EC=0x21, ELR=0x0) during normal typing speed (not rapid input as in the 2026-03-11 crash). The crash signature was identical — instruction abort at EL1 with null ELR — but the trigger conditions differed. The scene graph compositor migration had changed the userspace event loop structure.

### Bugs Found and Fixed

**Fix 12: Stale waiter registrations (correctness bug).**
When `sys_wait` takes the `BlockResult::Blocked` path, the thread is context-switched away. Unfired handle registrations (channel, timer, interrupt, thread exit, process exit waiters) remained live even after the thread was woken. On subsequent signals to those handles, the stale registrations could cause: (a) spurious wakeups with incorrect return values, (b) redundant wake attempts. The spurious wakeup issue explains the "spurious wakeup from sys_wait" bug noted in the Virtio-9P section — init's `wait(&[channel])` returning before the response was the stale waiter bug in action.

Fix: Added `stale_waiters` field to Thread. `complete_wait_for` now copies unfired entries to `stale_waiters` before clearing `wait_set`. At the start of the next `sys_wait`, stale waiters are unregistered from all handle types. Added `scheduler::take_stale_waiters()` to support this deferred cleanup pattern.

**Fix 13: Per-ASID TLB invalidation (performance + correctness hardening).**
`swap_ttbr0` used `TLBI VMALLE1IS` which flushes ALL TLB entries (both TTBR0 and TTBR1) across ALL cores on every context switch between different address spaces. This unnecessarily flushed kernel I-TLB entries on all cores, causing every core to re-walk page tables for kernel code. Under high context-switch rates (~400/sec), this created sustained TLB pressure that could transiently affect instruction fetch timing. While not a correctness bug per se, the unnecessary full flush was wasteful and the per-ASID alternative (`TLBI ASIDE1IS`) was already used correctly elsewhere in `address_space.rs::invalidate_tlb()`.

Fix: Changed `swap_ttbr0` to use `TLBI ASIDE1IS, <old_asid>` — invalidates only the outgoing ASID's TLB entries. Kernel TTBR1 entries and other processes' TTBR0 entries are preserved.

**Fix 14: Diagnostic assertions in exception handlers.**
Added `debug_assert` checks for null context pointers in `irq_handler`, `svc_handler`, and `user_fault_handler`. These will catch the null pointer at its source rather than manifesting as EC=0x21 in the exception vector. Also added a null check on the `schedule_inner` return value.

### Analysis

The original crash (ELR=0x0 at EL1) and the later variant (ELR=0x0A003A00) share the same root cause: **Fix 17 (TPIDR_EL1 race in `schedule_inner`, 2026-03-14).** When the scheduler lock drops after `schedule_inner` returns, IRQs are re-enabled. If a timer IRQ fires before exception.S updates TPIDR_EL1, `save_context` overwrites the old (parked) thread's Context with kernel-mode state. Fixes 12-14 addressed contributing factors but not the root cause. Fixes 5/6 (aliasing UB, nomem on DAIF) changed the timing enough to suppress most occurrences but didn't close the window.

Stress tested: 14,484 keys over 120 seconds (pre-Fix 17), 3000 keys at 1ms (post-Fix 17). No crash after Fix 17.

### Proactive Bug Hunting (continued, same session)

**Fix 15: sys::counter() nomem removed.** `mrs cntvct_el0` had `nomem` — same class as Fix 6. The counter is monotonically increasing hardware state; with `nomem`, LLVM could CSE or hoist repeated reads, returning stale timestamps.

**Fix 16: Trampoline `ldr [sp, #8]` nomem removed.** 18 thread trampolines across fuzz/fuzz-helper/stress used `options(nostack, nomem)` on `ldr [sp, #8]` — a memory read falsely declared non-memory. LLVM could reorder the preceding `write_volatile` of the argument past the load.

**Coverage analysis** identified critical gaps: `interrupt_register` (zero coverage), `handle_send` success path (zero), `interrupt_ack` success path (zero), `device_map` success path (zero), 9 syscalls never tested under concurrency. Addressed with 5 new fuzz phases (32–36):

- Phase 32: interrupt register/ack full lifecycle (register, ack, duplicate rejection, re-register after close, multi-IRQ, poll)
- Phase 33: handle_send success path (create channel → create child → send endpoint → child signals back via received handle)
- Phase 34: device_map success path (map UART0 MMIO, read register, error cases)
- Phase 35: concurrent scheduling context create/bind/borrow/return (4 threads, 100 ops each)
- Phase 36: concurrent DMA alloc/free (4 threads, 50 ops each, varied orders)

**Unsafe audit** reviewed all kernel modules with `unsafe` blocks: scheduler, address_space, memory, paging, per_core, thread_exit, process_exit. No new bugs found. Lock ordering verified correct.

**Kernel Change Protocol** codified in CLAUDE.md: nomem default-deny, SAFETY comments required, stress test mandate, anomaly tracking.

All 36 fuzz phases pass. 896 host tests pass. Build clean.

---

## Virtio-9P Host Filesystem Passthrough (2026-03-11)

**Status:** Working. Font loading end-to-end via 9P2000.L protocol over virtio transport.

**What was built:** Userspace virtio-9p driver (~450 lines) that reads files from the host macOS filesystem via QEMU's `-fsdev` passthrough. Implements 6 9P operations (version, attach, walk, lopen, read, clunk). Init sends a read request via IPC, the driver reads the file through 9P, fills a shared DMA buffer, signals back. Compositor loads the font from the runtime buffer instead of `include_bytes!`.

**Bugs found and fixed:**

1. **`payload_as`/`from_payload` hangs for large structs on aarch64 bare metal.** Both init and the 9p driver hung when using these helpers with FsReadRequest (60 bytes — full payload). ~~Root cause unclear.~~ **Retrospective (2026-03-13):** Likely caused by kernel Fix 5 (aliasing UB) or Fix 6 (`nomem` on DAIF) — both produced timing-dependent miscompilation at opt-level 3 that could manifest as hangs in userspace syscall paths. Evidence: `payload_as` uses `read_unaligned` internally and works correctly for 56-byte `CompositorConfig` structs in the compositor (added after the kernel fixes). The manual field-by-field workaround remains in the 9P driver and init but is no longer believed necessary. **Status: resolved** (root cause was kernel UB, not `payload_as`).
2. **Spurious wakeup from `sys_wait`.** Init's `wait(&[channel])` returned before the 9p driver sent its response. Fix: loop on wait+try_recv until the expected response message type arrives. ~~Root cause of the spurious wakeup not fully diagnosed.~~ **Retrospective (2026-03-13):** Root cause identified as kernel Fix 12 (stale waiter registrations). When `sys_wait` takes the `BlockResult::Blocked` path, unfired handle registrations remained live. A subsequent signal to one of those stale handles would spuriously wake the thread. The retry loop remains correct as defense-in-depth. **Status: resolved.**

**Architecture notes:**

- Chose 9P over virtiofs (simpler protocol, no host daemon, confirmed in QEMU).
- Shared host directory: `system/share/` — contains source-code-pro.ttf (9 KB).
- QEMU flags added to all 4 scripts (run, test, integration, crash).
- This validates the prototype-on-host strategy from Decision #16: implement Files against the host filesystem during prototyping, defer the real COW filesystem.

**Open questions for later:**

- Should investigate the `payload_as` hang root cause — it affects all IPC with large payloads.
- Should investigate the spurious wakeup root cause — retry loop works but masks a potential kernel bug.
- Phase 2 (general file service with event loop, multiple files) deferred.

---

## Bug Report: Kernel Crash Under Rapid Keyboard Input (2026-03-11)

**Severity:** High — kernel panic (instruction abort at EL1)
**Reproducible:** Yes — type rapidly into the QEMU window for ~10 seconds
**Introduced:** Not by new code (all new code is userspace). Exposed by the first sustained high-frequency event processing workload.

### Crash Signature

```console
💥 kernel sync: EC=0x21 ESR=0x86000006 ELR=0x0 FAR=0x0
instruction abort at EL1
```

- **EC=0x21**: Instruction abort from current EL (EL1 = kernel)
- **ELR=0x0**: Kernel tried to execute at address 0 — null function pointer
- **Metrics at crash:** ~440 ctx_sw/sec, ~370 syscalls/sec, 2595 ticks (~10.4s uptime)

### What's Happening

Three userspace processes run continuous event loops:

1. **Input driver**: `wait(IRQ)` → read event → `channel_signal(compositor)` → loop
2. **Compositor**: `wait(input_channel)` → render char → `channel_signal(GPU)` → loop
3. **GPU driver**: `wait(compositor_channel)` → transfer+flush (2 virtio cmds) → loop

Each keystroke triggers: IRQ → input wake → channel signal → compositor wake → render → channel signal → GPU wake → 2 GPU commands → loop. Under rapid typing, this produces ~50+ full cycles/sec, each involving:

- Vec alloc/free for `wait_set` (one per `sys_wait` call)
- 3+ lock acquisitions (scheduler, channel, timer)
- Thread state transitions (Ready → Running → Blocked → Ready)

### Suspect Analysis

**Suspect 1: Kernel heap allocator stress (MOST LIKELY)**
Each `sys_wait` call allocates a `Vec<WaitEntry>` via `store_wait_set`, and the wake path frees it via `wait_set.clear()` + drop. At ~370 syscalls/sec, the kernel heap allocator (linked-list first-fit with coalescing) handles hundreds of small alloc/free cycles per second. A coalescing bug under rapid free/alloc patterns could corrupt the free list, leading to a subsequent allocation returning corrupted memory. The null function pointer (ELR=0x0) is consistent with using a corrupted Box<Thread> where a vtable or function pointer has been zeroed.

_Test:_ Pre-allocate the wait_set Vec and reuse it across `sys_wait` calls (eliminate the alloc/free hotpath). If the crash disappears, it's the allocator.

**Suspect 2: Scheduler two-phase wake race**
`channel::signal()` collects the waiter ThreadId under the channel lock, releases it, then calls `try_wake_for_handle` under the scheduler lock. Between releasing the channel lock and acquiring the scheduler lock, another signal could arrive for the same thread. Both callers try to wake the same thread. `try_wake_impl` searches `blocked` by ThreadId and uses `swap_remove`. The second caller wouldn't find it (returns false) and falls through to `set_wake_pending_for_handle`. This _should_ be safe, but the swap_remove changes the order of the blocked list, which could interact badly with concurrent operations on other cores.

_Test:_ Add a serial print in `try_wake_impl` when a thread isn't found in blocked/running/ready (the fall-through case). If this fires frequently, it's the wake race.

**Suspect 3: Slab/heap interaction**
The kernel heap routes allocs by size: ≤2 KiB → slab, else → linked-list. The `Vec<WaitEntry>` starts small (8 bytes per entry × 1-2 entries = 16-32 bytes) and goes through slab. Rapid alloc/free of slab objects could expose a slab bug (double-free or free-list corruption).

_Test:_ Force Vec<WaitEntry> to allocate from the linked-list allocator by reserving a minimum capacity (e.g., `Vec::with_capacity(256)`). If the crash disappears, it's the slab.

### Fixes Applied (2026-03-11)

**Crash signature:** `ELR=0x0`, instruction abort at EL1. `ret` to address 0 on a valid kernel stack. Under rapid typing, crashed in ~15 seconds at opt-level 3.

**Root causes found (multiple):**

**Fix 1: Idle thread park (category b — intent not implemented).** `park_old()` comment said idle threads "go back to `cores[].idle`" but the code didn't do it. Fix: `park_old()` takes `core` parameter, restores idle threads. 17 scheduler state machine tests (`test/tests/scheduler_state.rs`).

**Fix 2: wait_set Vec reuse (category a — hot path allocation).** Each `sys_wait` allocated a fresh `Vec<WaitEntry>` + clone (~740 slab ops/sec). Fix: clear and repopulate `thread.wait_set` in-place, stack-allocated `[Option<WaitEntry>; 17]`. `push_wait_entry()` replaces `store_wait_set()`.

**Fix 3: Enhanced fault handler (permanent).** `kernel_fault_handler` now receives SP, LR (x30), TPIDR_EL1, thread ID, and saved Context fields from the assembly. Dumps stack words.

**Fix 4: Deferred thread drop (category a — use-after-free).** `park_old` dropped exited threads immediately, freeing kernel stack pages while `schedule_inner` was still executing on them. Fix: `State::deferred_drops` list, drained at start of next `schedule_inner`.

**Fix 5: Aliasing UB in syscall dispatch (category a — `&mut *ctx` vs `&mut State`).** `dispatch()`, `sys_wait()`, `sys_futex_wait()`, and `block_current_unless_woken()` created `&mut *ctx` references that aliased with the scheduler lock's `&mut State` (both cover the same Thread Context). With inlining at opt-level 3, LLVM saw two `noalias` mutable references to overlapping memory → miscompilation. Fix: all Context access through `ctx` now uses `core::ptr::addr_of!` + raw pointer reads/writes. New `dispatch_ok()` + `result_to_u64!` replace the old `dispatch_syscall!` macro.

**Fix 6: `nomem` on IrqMutex DAIF asm (PRIMARY FIX — category a).** `options(nostack, nomem)` on `mrs daif` / `msr daifset` / `msr daif` in `sync.rs` told LLVM these instructions don't access memory. This allowed LLVM to reorder lock-protected memory operations past the interrupt masking boundary, creating a race where accesses occurred with interrupts enabled on SMP. Fix: removed `nomem` from all DAIF manipulation and system register writes (`msr tpidr_el1`, `msr daifclr`). **This was the main fix — crash time went from ~15s to ~100-188s.**

**Fix 7: `#[inline(never)]` on all scheduler public functions.** Prevents LLVM from inlining scheduler internals into syscall/IRQ handlers, reducing the optimization surface for aliasing exploitation. Cheap (one `bl` instruction per scheduler call, dominated by IrqMutex lock cost).

**Fix 8: Automated crash test (`crash-test.sh`).** Launches QEMU headless, sends rapid keyboard input via monitor socket (Python + Unix socket), monitors serial output for crash. Usage: `./crash-test.sh [seconds]`.

**Remaining issue (RESOLVED 2026-03-11, follow-up session):** The residual opt-level 2-3 crash was originally observed only via manual keyboard typing in the QEMU window. The headless stress test (50M iterations, 137s, 4 SMP cores, opt-level 3) passes consistently. The automated crash test via AppleScript was a flawed methodology — it depends on macOS display routing and QEMU window focus, which introduces timing variability unrelated to the kernel. The headless stress test saturates the exact same syscall paths (channel_signal/wait, timer create/destroy, scheduling context switches) at much higher rates than keyboard input ever could. **Opt-level 3 is safe for use.** All 11 fixes (especially Fix 5: aliasing UB and Fix 6: nomem on DAIF) resolved the underlying issues.

**Diagnostic investigation trail:** (1) schedule_inner elr=0 check → never fired. (2) SP capture → valid kernel stack. (3) LR=0 → confirmed `ret` to null. (4) x30=0 check → false positive for EL0 threads. (5) Thread Context dump → saved Context always valid. (6) ELR verification in assembly → didn't trigger (crash is NOT from eret). (7) opt-level bisection → opt-level 1 passes, 2-3 crash. (8) `#[inline(never)]` bisection → scheduler inlining contributes. (9) `nomem` removal → main fix.

### Additional Hardening (2026-03-11, continued)

**Fix 9: `nomem` removal across all inline asm.** Systematic audit of all 99 `unsafe` blocks. Removed `nomem` from:

- `timer.rs`: `msr cntp_tval_el0` (timer reprogram), `mrs cntpct_el0` (counter read), `msr cntp_ctl_el0` (timer enable), `msr daifclr` (IRQ unmask)
- `power.rs`: `hvc #0` (PSCI CPU_ON — boots secondary cores)
- `syscall.rs`: merged split AT+MRS asm blocks into single blocks (address translation + PAR_EL1 read must not be reordered)

**Fix 10: Headless stress test (`stress-test.sh` + `user/stress/main.rs`).** Userspace stress program exercises IPC ping-pong, timer churn, and allocator pressure without needing a display or keyboard. Integrated into build system (`build.rs`, `init/main.rs`). Usage: `./stress-test.sh [seconds]`.

**Fix 11: Property-based scheduler tests (`test/tests/scheduler_state.rs`).** 3 new tests added to existing 17:

- `randomized_scheduler_state_machine`: 500 random actions × 50 seeds, checks invariants after every action
- `rapid_block_wake_never_duplicates`: rapid block/wake cycles never create duplicate thread entries
- `all_threads_eventually_reaped`: exited threads are always cleaned up via deferred drops

Total: 20 scheduler state machine tests, all passing.

### How to Test

```sh
cd system
./crash-test.sh 120   # Automated: 120 seconds of rapid keyboard input
./stress-test.sh 30   # Headless stress test (no display needed)
cargo run --release    # Manual: type rapidly in QEMU window
cd test && cargo test scheduler_state  # Property-based scheduler tests
```

---

## Open Threads

Active questions we've started exploring but haven't resolved. Each thread links to the decisions it would inform.

### AA transition softness tuning

**Informs:** Phase 4 visual polish
**Status:** Deferred — not blocking, aesthetic preference
**Context:** Our analytic rasterizer produces mathematically correct coverage with "hard" AA transitions (fewer partial-coverage pixels at edges). macOS Core Text has slightly softer/wider transitions that give text a warmer feel. Options: post-rasterization Gaussian blur on coverage, wider filter kernel in rasterizer, or leave as-is (sharper is arguably better for a code editor). Decision: leave for now, revisit if the aesthetic feels wrong once more of the UI is built.

### Italic font rendering

**Informs:** v0.5 (Rich inline text / multi-style runs)
**Status:** Deferred — blocked on style runs
**Context:** Italic variants are loaded (inter-italic.ttf, jetbrains-mono-italic.ttf). Rasterizer and shaping engine handle any font data. Missing piece: a way to select italic in the scene graph. Requires content-type or style metadata → italic font data mapping, which depends on multi-style text runs (v0.5 scope). No work needed now — infrastructure is ready.

### Is "compound" intrinsic or contextual?

**Informs:** Decision #14 (Compound Documents), Glossary
**Status:** Partially resolved by uniform manifest model (2026-03-09)
**Context:** With uniform manifests, every document has a manifest. A PDF becomes a document whose manifest references a single content file. "Compoundness" is a property of the manifest's structure (how many content references), not an intrinsic property of the content format. If the user decomposes a PDF for part-by-part editing, a new manifest with extracted parts could be created. Remaining question: is decomposition automatic, user-initiated, or editor-driven?

### ~~Referenced vs owned parts in documents~~ — SETTLED (copy semantics)

**Informs:** Decision #14 (Compound Documents)
**Status:** Settled (2026-03-11). Copy semantics with provenance metadata.
**Context:** Explored three approaches: reference (shared, changes propagate), copy (independent, self-contained), and copy-on-reference hybrid (start shared, diverge on first edit). References require tracking a global reference graph (delete checks, broken links, spooky action at a distance). The hybrid creates invisible state — whether editing affects the original depends on whether you've previously edited it in this context, which the user can't predict. Copy semantics won: each compound document gets its own copy of embedded content. Self-contained, no reference tracking, no broken links, no surprising behavior. COW at the filesystem level means "copies" share physical blocks until they diverge — logical independence with physical efficiency. The original document's ID is stored as provenance metadata, enabling an explicit "update to latest" action (pull, not push). One-directional: the compound knows about the original, the original doesn't know about the compound. Same parent→child / child-doesn't-know-parent isolation as the compound document model generally.

### OS service interface map

**Informs:** Decisions #9, #7, #14, #15, #17
**Status:** Preliminary mapping done (2026-03-09), no interfaces designed yet
**Context:** Mapped all inter-component interfaces by boundary. The OS service is where interface design effort concentrates — edit protocol, metadata queries, interaction model, translator interface. The kernel surface (12 syscalls) is small and stable. Internal OS service interfaces (renderer, layout engine, compositor, scheduling policy) matter for implementation but can evolve freely. Key finding: scheduling policy needs no separate interface (falls out of edit protocol + kernel syscalls). Web engine adapter is not separate from translator interface. See insights log for full table.

### Shell architecture and system gestures

**Informs:** Decision #17 (Interaction Model), OS service interface design
**Status:** Under active exploration (2026-03-10/11)
**Context:** The shell's architectural placement was explored across two sessions. Key findings:

1. **Blue-layer symmetry:** Trust (kernel/OS service/tools) and complexity (red/blue/black) are orthogonal axes. The blue adaptation layer wraps the core on all sides — drivers below (adapt hardware), translators at sides (adapt formats), editors + shell above (adapt users). Editors are "user drivers."

2. **Shell is blue-layer but not purely modal.** Initially proposed the shell as an untrusted tool identical to editors, active when no editor is (modal). But switching documents while in an editor requires the shell to intercept input — so the shell is ambient, not modal. Revised model: system gestures (switch, invoke search, close) baked into OS service input routing (always work, not pluggable); navigation UI (what search looks like, document list) provided by shell (pluggable, restartable).

3. **One-document-at-a-time leaning.** UI model closer to macOS fullscreen Spaces than windowed desktop. View one document at a time, switch through the shell. Not settled.

4. **Compound document editing tension.** "Editors bind to content types" + "one editor per document" conflict for compound documents. Initial instinct: editor nesting (same text editor used within presentations, standalone text docs, etc.). But nesting creates complexity. Unresolved — needs dedicated exploration.

Open questions: system gesture vs shell input boundary, compound editor nesting model, whether content-type interaction primitives (cursor/selection/playhead from OS service) need to become richer editing primitives for compound documents to work.

### GPU path rendering via stencil-then-cover

**Informs:** Decision #11 (Rendering Technology), virgil-render driver
**Status:** Approach settled, implementation deferred until core emits `Content::Path`
**Context:** Researched four approaches to GPU path rendering (2026-03-17). Stencil-then-cover is the right technique for the current virgl stack (ES 3.0 ceiling from ANGLE/Metal). Compute-shader rendering (Vello-style) requires ES 3.1 + SSBOs, blocked by ANGLE's hardcoded `kMaxSupportedGLVersion = 3.0`. virglrenderer's protocol supports compute (`VIRGL_CCMD_LAUNCH_GRID`, `SET_SHADER_BUFFERS`) but the host GL doesn't expose it. Compute becomes viable on real hardware with a native driver (no ANGLE translation). Currently deferred: core never emits `Content::Path` — the Phase 2 rendering redesign eliminated SVG paths, icons use the glyph cache. See full journal entry above for details and implementation plan.

### View/edit in the CLI

**Informs:** Decision #17 (Interaction Model)
**Status:** Briefly mentioned, not explored
**Context:** The view/edit distinction is clear in GUI. How does it translate to CLI? Tools-as-subshells? Read-commands-always-safe? The CLI and GUI are equally fundamental interfaces (Belief #4), so the CLI can't be an afterthought.

### ~~Kernel architecture~~ — SETTLED (microkernel)

**Informs:** Decision #16 (Technical Foundation)
**Status:** All sub-decisions settled except filesystem COW on-disk design. Kernel is a microkernel by convergence.
**Context:** From-scratch Rust kernel on aarch64. Microkernel: address spaces, threads, IPC, scheduling, interrupt forwarding, handles. All semantic code in userspace. Settled: soft RT, no hypervisor (EL1), preemptive + cooperative yield, traditional privilege (EL0), split TTBR, handles, ELF, ring buffer IPC, three-layer process arch, SMP (4 cores), EEVDF + scheduling contexts, userspace drivers (MMIO mapping + interrupt forwarding), userspace filesystem (kernel owns COW/VM, filesystem manages on-disk layout). Remaining: filesystem COW on-disk design.
**Leaning — syscall API:** 12 syscalls in three families. Handle family: `wait(handles[])`, `close(handle)`, `signal(handle)`. Synchronization: `futex_wait`, `futex_wake`. Scheduling: `sched_create/bind/borrow/return`. Plus lifecycle: `exit`, `yield`, `write` (debug, temporary). Generic verbs on typed handles — handle type carries context. `wait` subsumes old `channel_wait` and gains multiplexing. OS service uses reactive/stream composition on top of `wait`. See insights log for full rationale.

### Display engine architecture

**Informs:** Decision #11 (Rendering Technology), Decision #15 (Layout Engine), Decision #17 (Interaction Model)
**Status:** Complete (2026-03-10). All three build steps done. Full display pipeline working end-to-end.
**Context:** Graphical output on QEMU virt. virtio-gpu (paravirtual, 2D protocol) reuses existing virtio infrastructure. Key architectural conclusions:

- **Surface-based trait, not framebuffer.** A raw framebuffer (`map() → &mut [u8]`) is specific to software rendering — GPU acceleration means the CPU never touches pixels. The universal abstraction is surfaces and operations: `create_surface`, `destroy_surface`, `fill_rect`, `blit`, `present`. The driver implements this trait; whether it uses CPU loops or GPU commands internally is the driver's business.
- **Display vs rendering are separate concerns in one device.** Display = get a buffer to the screen (last mile). Rendering = fill the buffer (compositing). Both always happen. GPU acceleration changes who fills the buffer (GPU vs CPU), not the display path. A GPU chip does both; one driver.
- **Three components, one interface.** Compositor (above) works with surfaces, calls trait methods. Driver (below) translates trait methods to hardware operations. The trait is the boundary — a contract, not a component. The compositor doesn't know if the driver uses CPU loops, GPU commands, or anything else. Software rendering is a fallback strategy inside the driver, not a separate thing the OS selects.
- **virtio-gpu overhead is inherent, not architectural.** Performance hit is the VM boundary (guest→host copy). With real hardware, the display controller reads directly from the buffer via DMA scanout — no copy. The abstraction doesn't add overhead; virtio does.
- **Build plan:** (a) virtio-gpu userspace driver ✅, (b) drawing primitives + bitmap font ✅, (c) toy compositor ✅. All done. Everything above the driver is portable to real hardware.
- **Step (a) done (2026-03-10):** `system/services/drivers/virtio-gpu/main.rs`. All 6 core 2D commands. Test pattern at 1280x800.
- **Step (b) done (2026-03-10):** `system/libraries/drawing/` — pure no_std drawing library. Surface abstraction with RGBA canonical format (encode/decode at pixel boundary). Primitives: fill_rect, draw_rect, draw_line (Bresenham), draw_hline/vline, set/get_pixel, blit. Embedded 8×16 VGA bitmap font with draw_glyph/draw_text. 41 host-side tests.
- **Step (c) done (2026-03-10):** `system/services/compositor/main.rs` — toy compositor draws demo scene (title bar, 3 colored panels with text, status bar) into shared framebuffer. `system/services/init/main.rs` — proto-OS-service that embeds all ELFs, reads device manifest, spawns all processes, orchestrates display pipeline. Kernel `memory_share` syscall (#24) enables zero-copy framebuffer sharing. Full pipeline: init → DMA alloc → share with compositor → compositor draws → signal → GPU driver presents → pixels on screen.
- **Alignment bug found (2026-03-10):** u64 `read_volatile` from 4-byte-aligned address is UB in Rust. Caused silent process death. Fixed by padding device manifest entries to 8-byte alignment. User fault handler didn't print diagnostic before killing process — known kernel bug.

Open questions: exact surface trait API, double buffering strategy, font choice for production (Spleen PSF2 over hand-rolled VGA font), trait naming.

### Compositor design

**Informs:** Decision #11 (Rendering Technology), Decision #14 (Compound Documents), Decision #15 (Layout Engine), Decision #17 (Interaction Model)
**Status:** Mental model established (2026-03-10), toy compositor implemented (2026-03-10)
**Context:** Explored the compositor's role, architecture, and how it differs from traditional desktop compositors. Key findings:

1. **Compositor = function from surface tree to pixel buffer.** Structurally identical to React's render pipeline: declarative tree (manifest = component tree) → damage calculation (= reconciliation/diff) → minimal pixel updates (= commit). The document manifest IS the scene graph.

2. **Scene graph is a tree shaped by document structure.** Not a flat list of overlapping windows. Compound documents create nested surfaces (chart within a slide within a presentation). The compositor embodies the document's structure. Tree is narrow and deep (one document, nested content parts) vs traditional desktop which is wide and shallow (dozens of top-level windows).

3. **Z-overlap is dramatically simpler.** Traditional desktops: 30+ arbitrary overlapping windows. Our OS: 1 document + maybe 1-2 floating elements + system UI = 3-4 z-layers total. No occlusion culling, no complex z-ordering. Structural constraints eliminate the problem rather than clever algorithms solving it.

4. **Two surface behaviors: contained and floating.** Most content is contained (clipped to parent, positioned by layout engine). Some needs to float: drag ghosts, popovers, tooltips, editor overlays, transitions. Floating surfaces are rendered above the normal tree, not clipped. Similar to Wayland subsurfaces vs popups, or CSS normal flow vs position:fixed.

5. **Compositor↔GPU driver data path matters.** Three options explored: (a) copy via IPC (too slow for framebuffer-sized data), (b) shared memory (correct — zero-copy, needs kernel Phase 7), (c) same process (simple but couples trust levels). Real systems use (b). For toy compositor, temporary coupling is OK with eyes open about what's scaffolding.

6. **"Informed" vs "blind" compositor.** Traditional compositors are blind — each app is an opaque pixel rectangle. Ours is informed — knows document structure, content types, document state. Enables: damage prediction (text cursor = known small rectangle), update cadence optimization (video at 24fps, static text at 0fps), content-type-aware rendering priority.

7. **Compound documents ARE nested windowing.** A presentation with an embedded chart is structurally identical to a window containing a sub-window. The differences: layout determines position (not user dragging), no chrome, nesting is content-driven. But the compositor's internal model needs the same tree structure. This connects directly to the unresolved compound document editing tension.

8. **Dragging in absolute layouts = window dragging.** Canvas/freeform layouts (Decision #14 spatial axis) let users drag content. During drag: compositor moves surface in real time. On drop: editor commits via beginOp/endOp. Same pattern as traditional window management.

9. **Pure containment is too rigid.** Pop-out editing (drag photo out to adjust), tooltips extending beyond parent, transitions between containers — all need floating surfaces. Don't commit to pure containment. Cost of floating support is low (extra render pass), UI cases are real.

**Implementation note (2026-03-11, commit 827bcc8):** The compositor now demonstrates the settled editor separation architecture. It receives input from the input driver, routes it to the text editor via IPC, receives write requests back, and applies them as sole writer to the document buffer. Four processes in the display pipeline (GPU driver, input driver, text editor, compositor). The compositor↔editor channel is bidirectional: input events out, write requests in. This validates the "OS service is sole writer" design in running code.

Open questions: exact scene graph API, how layout engine and compositor interface (does layout produce the tree that compositor renders?), React-style damage diffing (how much complexity is justified for 3-4 z-layers?), whether compositor is part of OS service or separate. Shared memory is no longer blocked — kernel Phase 7 (`memory_share` syscall #24) is done.

### COW Filesystem

**Informs:** Decision #16 (Technical Foundation — filesystem sub-decision), Decision #12 (Undo), Decision #14 (virtual manifest rewind)
**Status:** Interface designed (2026-03-11). Placement settled (userspace service). On-disk format deferred — prototype-on-host strategy adopted. See `design/research/cow-filesystems.md`.
**Context:** Studied RedoxFS (Rust, COW but no snapshots), ZFS (birth time + dead lists = gold standard for snapshots), Btrfs (refcounted subvolumes), Bcachefs (key-level versioning). Key findings: (1) birth time in block pointers is non-negotiable for efficient snapshots, (2) ZFS dead lists make deletion tractable, (3) per-document scoping needed (datasets/subvolumes, not whole-FS snapshots), (4) `beginOp`/`endOp` maps naturally to COW transaction boundaries. TFS (Redox's predecessor) attempted per-file revision history but didn't ship it — cautionary data point. Filesystem is a userspace service — kernel owns COW/VM mechanics (page fault handler), filesystem manages on-disk layout (B-trees, block allocation, snapshots). **New constraint (2026-03-09):** metadata DB must live on the COW filesystem so its historical state is preserved in snapshots — required for uniform rewind performance across static and virtual documents. Time-correlated vs per-document snapshots still open — per-document snapshots + a COW'd metadata DB might be sufficient for world-state queries without coordinated global snapshots. Needs further exploration.

**Files interface settled (2026-03-11).** 12 operations: create, clone, delete, size, resize, map_read, map_write, snapshot, restore, map_snapshot, snapshots (list), delete_snapshot, flush. Deliberately absent: paths/directories (files addressed by ID), permissions (OS service is sole consumer), extended attributes (metadata lives in DB), file locking (OS service serializes writes via event loop), links (copy semantics via clone), rename (metadata DB concern), batch operations (OS service sequences), file type info (metadata DB concern). The interface is a dumb file store — it knows nothing about documents, undo ordering, or compound structures. See journal insights for full design rationale.

**Prototype-on-host strategy (2026-03-11).** Implement the Files interface against macOS (regular files + file copies for snapshots + mmap). Build the real COW filesystem later once the interface is proven through actual editor/undo/document pipeline usage. The filesystem's on-disk format is a leaf node — complex inside, simple interface. Same pattern as the rendering engine decision (settle the architecture, defer the implementation).

**Prototype validated (2026-03-11).** `prototype/files/` — HostFiles implementation backed by macOS filesystem, 21 tests covering all 12 operations (create, clone, delete, size, resize, map_read, map_write, snapshot, restore, map_snapshot, snapshots, delete_snapshot, flush). Tests include: basic CRUD, snapshot chains, restore, clone independence, write-at-offset, resize, error cases, undo workflow simulation. Interface is proven through concrete implementation; ready for integration when the OS service pipeline reaches persistent document storage.

Open questions: on-disk format (deferred), snapshot naming, pruning policy, page cache placement, snapshot scope (global vs per-document vs time-correlated — punted, doesn't block interface).

### Virtual manifests, retention, and the OS-as-document

**Informs:** Decision #14 (Compound Documents), Decision #17 (Interaction Model), Decision #16 (COW filesystem)
**Status:** Core concepts settled (2026-03-09): static/virtual manifests, retention policies replacing transient concept, streaming as virtual. OS-as-document not yet committed.
**Context:** Manifests can be static (disk-backed, COW'd) or virtual (content generated on demand from internal state OR external sources). Virtual manifests enable: system-derived documents (inbox, search results, dashboard — internal state), streaming content (YouTube — external source). All documents are persistent — no "transient" concept. Retention policies handle cleanup (webpages 30 days, user content permanent). COW pruning system manages both edit history and document lifecycle. The OS itself could be presented as a document or query (shell/GUI as editors/viewers) — potentially informs Decision #17. Virtual documents inherit time-travel from underlying static documents' COW history. Design constraint: rewind performance must be uniform (metadata DB on COW filesystem). "Transient documents" concept explored and rejected — it's a retention policy, not a document type.

### ~~Privilege model (EL1 / EL0 boundary)~~ — SETTLED

**Resolved:** Traditional — all non-kernel code at EL0. One simple boundary, one programming model. Consistent with Decision #4 (simple connective tissue) and Decision #3 (arm64-standard interface). Language-safety (B) rejected as unsolved research problem for extensibility. Hybrid (C) rejected as two-ways-to-do-the-same-thing. See Decision #16 in decisions.md.

### ~~Address space model (TTBR0 / TTBR1)~~ — SETTLED

**Resolved:** Split TTBR — TTBR1 for kernel (upper VA), TTBR0 per-process (lower VA). Follows directly from the traditional privilege model. See Decision #16 in decisions.md.

---

## Discussion Backlog

Topics to explore, roughly prioritized by which unsettled decisions they'd inform. Not a task list — a menu of interesting conversations to have when the urge strikes.

### High leverage (unblocks multiple decisions)

1. ~~**Rendering technology deep dive** (Decision #11)~~ — **SETTLED.** Existing web engine integrated via adaptation layer. Key insight: a webpage IS a compound document (HTML=manifest, CSS=layout, media=referenced content) — can be handled through the same translator pattern as .docx. Rendering direction open (web engine renders everything vs. native renderer with web content translated inward). Engine complexity pushed into the blue adaptation layer. Prototype on macOS. See Decision #11 in decisions.md.

2. ~~**What does the IPC look like?**~~ (Decision #16) — **SETTLED.** Shared memory ring buffers with handle-based access control. One mechanism for all IPC. Kernel creates channels and validates messages at trust boundaries, but is not in the data path. Documents are memory-mapped separately. Editor ↔ OS service ring buffers carry control messages only: edit protocol (beginOp/endOp), input events, overlay descriptions. Metadata queries use a separate interface (not the editor channel — different cadence, potentially large results). Three-layer process architecture: kernel (EL1) + OS service (EL0, trusted, one process for rendering + metadata + input + compositing) + editors (EL0, untrusted). See Decision #16 in decisions.md.

3. **The interaction model** (Decision #17) — What does using this OS actually feel like? Mercury OS and Xerox Star are reference points. How do you find documents? What does "opening" something look like? How do queries surface in the GUI?

### Medium leverage (deepens settled decisions)

4. **Compound document authoring workflow** — We know the structure (manifests + references + layout), but how does a user actually _create_ a compound document? Do they start with a layout and add content? Does it emerge from combining simple documents?

5. **Content-type rebase handlers in practice** — We know the theory (git merge generalized). What would a text rebase handler actually look like as an API? What about images? This would validate the edit protocol's upgrade path.

6. **The metadata query API** — Decision #7 settled on "simple query API backed by embedded DB." What does this API actually look like? What are the verbs? How does it feel to use from both GUI and CLI?

6b. **IANA mimetype → OS document type mapping** — Systematic exercise: map common IANA mimetypes to OS document types, relationship axes, and editor bindings. Which mimetypes map to single-content documents (image/png → image document)? Which suggest compound documents (text/html → compound with flow layout)? What are the OS-native mimetypes for compound document types (presentation, project, album)? This would validate the three-axis model against real content types and surface edge cases. Connects to the mimetype-of-the-whole question (partially resolved) and content-type registration via editor metadata.

### Exploratory (interesting but less urgent)

7. **Historical OS deep dives** — Plan 9's /proc and per-process namespaces. BeOS's BFS attributes in practice. OpenDoc's component model and why it failed. Xerox Star's property sheets. Each could inform current design.

8. ~~**Scheduling algorithm**~~ — **SETTLED.** EEVDF + scheduling contexts (combined model). EEVDF provides proportional fairness with latency differentiation (shorter time slice = earlier virtual deadline). Scheduling contexts are handle-based kernel objects (budget/period) providing temporal isolation between workloads. Context donation: OS service borrows editor's context when processing its messages (explicit syscall). Content-type-aware budgeting: OS service sets budgets based on document mimetype and state. Best-effort admission. Shared contexts across an editor's threads. See Decision #16 in decisions.md.

9. **The "no save" UX** — We committed to immediate writes + COW. What does this feel like for content that's expensive to re-render? What about "I was just experimenting, throw this away"? Is there a need for explicit "draft mode" or does undo cover it?

10. **Editor plugin API design** — What's the actual interface between an editor plugin and the OS? How does an editor register, receive input, draw overlays? This is where the abstract editor model becomes concrete. The IPC ring buffer between editor ↔ OS service is essentially an RPC transport (msg_type = function name, payload = arguments). The API question is: what are the RPCs?

### Overlay protocol

**Informs:** Editor plugin API (#10), Rendering technology (#11)
**Status:** Three options identified, not yet committed
**Context:** Editors need to show tool-specific visual feedback (crop handles, selection highlights, brush preview, text cursor) without owning any rendering surface. Options:

- **A. Semantic overlays:** OS defines ~10-15 meaningful types (cursor, selection, bounding-box, guide-line, tool-preview). Editor says "selection is offsets 10-50," OS decides how to render. Scalable set, consistent styling, but limits editors to predefined vocabulary.
- **B. Overlay as mini-document:** Overlay is a small scene graph / SVG-like document in shared memory. Editor writes to it, OS renders. Ring buffer carries only "overlay updated" notifications. Most document-centric option.
- **C. Pixel buffer:** Editor gets a shared-memory pixel buffer, renders its own overlay, OS composites. Most flexible, but conflicts with "OS renders everything."
- **Hybrid A+B:** Semantic overlays for 90% case + custom overlay document escape hatch for exotic tool UI. Seems promising.

### Metadata query routing

**Informs:** File organization (#7), Interaction model (#17)
**Status:** Clarified — metadata queries don't belong in editor ↔ OS service ring buffer
**Context:** Metadata queries (search by tags, attributes, etc.) are request/response, potentially large results, not real-time. They're primarily a shell/GUI → OS service concern, not an editor concern. Should use a separate interface — possibly a separate channel type, or results as memory-mapped documents. The editor ↔ OS service channel carries only: input events, edit protocol, overlays.

---

## Insights Log

Non-obvious realizations worth preserving. These are the "aha moments" that should inform future design thinking.

### The prototype validates the interface, not the technique (2026-03-17)

The thick driver architecture means rendering strategy is a driver-internal choice invisible to the rest of the OS. Stencil-then-cover on QEMU/virglrenderer proves paths flow correctly through the scene graph → driver → display pipeline. A native GPU driver on real hardware could switch to compute-shader path rendering without changing a single line outside the driver. This is the same principle as "settle the approach, not the technology" (Decision #11) — the scene graph interface is the design artifact, the rendering technique is a leaf node behind it. Prototyping with ANGLE's ES 3.0 ceiling doesn't constrain the design; it only constrains which rendering techniques the prototype can validate.

### The architecture is the interfaces, not the components (2026-03-16)

A rendering pipeline is a series of data shape transformations. Each data shape (Hardware Events, Key Events, Write Requests, Scene Tree, Pixel Buffer) is an interface — the contract between two components. The "components" (Input Driver, Editor, Core, Render Backend) are just translation layers between interfaces. Data of one shape goes in, data of another shape goes out, the logic is fully encapsulated. This framing eliminates confusion about "where does X live?" — if it transforms the data shape, it's inside the translator. If it changes the shape itself, it's an interface redesign. Thinking in interfaces rather than components also makes it obvious when a component is doing too much: the compositor was translating Scene Tree → Pixels while also understanding text, parsing SVG, and managing font caches. That's not one translation — it's several, jammed into one box.

### Glyphs are cached vector shapes, not text (2026-03-16)

The `Glyphs` scene graph content type isn't "text" — it's "cached vector shapes looked up by ID from a collection." Text is the most common use case, but monochrome icons are structurally identical: a collection of vector outlines indexed by ID. An icon set goes through the exact same pipeline as a font — same glyph cache, same rasterization, same compositing. This eliminated the 795-line SVG parser in the compositor. When an abstraction naturally absorbs use cases you didn't explicitly design for, the boundary is probably in the right place.

### Decomposition is a spectrum, not a binary (2026-03-05)

Any content decomposes further — video into frames, text into codepoints, codepoints into bytes. Taken to its conclusion, everything is Unix. The OS draws its line at the mimetype level (anchored to IANA registry), same way Unix draws at the byte level (anchored to hardware). This isn't arbitrary — it's pragmatic and externally anchored.

### Selective undo and collaboration are the same problem (2026-03-05)

Both require rebaseable operations. Building content-type rebase handlers unlocks both. This means collaboration isn't a separate feature to "add later" — it's a natural consequence of investing in selective undo.

### Total complexity is conserved (2026-03-05)

External complexity is fixed. Making the core simpler by pushing everything into adapters doesn't reduce complexity — it displaces it. L4 microkernel is the cautionary tale. The design metric is minimizing total irregularity across core + adaptation layer jointly. This should directly inform the kernel architecture decision.

### Modal tools eliminate an entire problem class (2026-03-05)

One editor at a time means no concurrent composition, no operation merging, no coordination protocol. The "pen on desk" metaphor isn't just UX — it's an architectural simplification that removes the hardest part of the edit protocol.

### application/octet-stream is self-penalizing (2026-03-05)

The escape hatch back to Unix-level agnosticism exists, but using it means losing everything the OS provides. The system doesn't need to forbid bypassing the type system, because bypassing it is its own punishment.

### Hard RT costs are user-visible, not just developer-visible (2026-03-06)

Hard realtime doesn't just make the OS harder to build — it makes it worse for desktop use. Throughput drops (scheduler constantly servicing RT deadlines), low-priority tasks starve under high-priority load, and dynamic plugin loading fights provable timing bounds (can't admit code without timing analysis). Critically, soft RT is perceptually indistinguishable from hard RT for audio/video on modern hardware (sub-1ms scheduling latency vs ~5-10ms human perceptual threshold). Hard RT is for physical-consequence domains (medical, automotive, aerospace), not desktops.

### Preemptive and cooperative are complementary, not a binary (2026-03-06)

The edit protocol's beginOperation/endOperation boundaries are natural cooperative yield points. Preemptive scheduling is the safety net (buggy editor can't freeze system). Both work together: preemptive as the ceiling, cooperative as the efficient path. The full context save/restore infrastructure supports preemption; cooperative yield is purely additive — no rework needed.

### Hypervisor IPC works against "editors attach to content" (2026-03-06)

A hypervisor-based isolation model (editors in separate VMs) requires VM-exit/enter for every cross-boundary call. This directly conflicts with the immediate-write editor model — every `beginOperation`/write/`endOperation` would cross a VM boundary. The thin edit protocol's value comes from low overhead; VM transitions are the opposite of low overhead. Hardware isolation at the EL1/EL0 boundary (syscalls) is a much lighter mechanism for the same goal.

### Centralized authority simplifies access control (2026-03-06)

Full capability systems (seL4, Fuchsia) solve distributed authority — many actors granting, delegating, and revoking access to each other. This OS is architecturally centralized: the OS mediates all document access, renders everything, manages editor attachment. In a centralized-authority model, OS-mediated handles (per-process table, integer index, rights check) provide the same security guarantees as capabilities with far less machinery. Handles enforce view/edit and the edit protocol at the kernel level. The query/discovery tension that plagues capabilities (how do you search for documents you don't have capabilities to?) doesn't arise because the query system is OS-internal. Handles can extend to IPC endpoints and devices incrementally — growing toward capabilities only if distributed authority is ever needed.

### "OS renders everything" produces three-layer architecture (2026-03-07)

"The OS renders everything" is a design principle. "Rendering code should not be in the kernel" is an engineering constraint. Together they force a three-layer architecture: kernel (EL1, hardware/memory/scheduling/IPC), OS service (EL0, rendering/metadata/input/compositing), editors (EL0, untrusted tools). The primary IPC relationship is editor ↔ OS service — not "everything through the kernel." The kernel's IPC role is control plane (setup, access control, message validation), not data plane (actual byte transfer).

### Top-down design explains why content-type awareness is load-bearing (2026-03-08)

Most OSes are designed bottom-up: start from hardware, build abstractions upward. Unix asked "what does the PDP-11 give us?" → bytes → files → processes → pipes. The user-facing model is whatever the hardware abstractions naturally produce. This OS is designed top-down: start from the user experience ("what should working with documents feel like?") and work down toward hardware. Content-type awareness isn't an independent axiom — it's what you discover when user-level requirements (viewing is default, editors bind to content types, undo is global) flow down to the system level. It shows up in rendering, editing, undo, scheduling, file organization, and compound documents because every subsystem was designed to serve the user-level model, not the hardware-level model. Previous document-centric OSes (Xerox Star, OpenDoc) stopped at the UX — "documents first" but the kernel, scheduler, and filesystem remained content-agnostic. This OS takes document-centricity seriously at the system level, which is why content-type awareness permeates everywhere. The methodology (top-down) produced the principle (content-type awareness) as a natural consequence.

### Content-type awareness is a scheduling advantage (2026-03-08)

A traditional OS has no idea what a process is doing. Firefox playing video and Firefox rendering a spreadsheet look identical to the scheduler. Application developers manually request RT priority (and often get it wrong). This OS knows the mimetype of every open document. The OS service creates scheduling contexts for editors and sets budgets based on content type: tight period for `audio/*` playback, relaxed for `text/*` editing, trickle for background indexing. More importantly, the OS service knows document _state_ — video being played gets RT budget, video paused on a frame drops to background levels. The scheduling context isn't set once; the OS service adjusts it dynamically. This is the document-centric axiom paying dividends in an unexpected place: "OS understands content types" was a decision about file organization and viewer selection, but it turns out to be a scheduling decision too.

### Handles all the way down: memory, IPC, time (2026-03-08)

With scheduling contexts as handle-based kernel objects, three fundamental resources use the same access-control model: memory (address space), communication (channel), and time (scheduling context). This consistency makes the design feel inevitable rather than assembled. Each resource is created by the kernel, held via integer handle, rights-checked on use, and revocable. The pattern was adopted for IPC (forced by the access-control decision), then extended to scheduling because the domains were similar enough — the adoption heuristic in action.

### Ring buffers only carry control messages because documents are memory-mapped (2026-03-07)

The highest-bandwidth data in a typical OS (rendering surfaces, file contents) doesn't flow through IPC in this design. The OS service renders internally (no cross-process rendering surfaces). Documents are memory-mapped by the kernel into both OS service and editor address spaces (no file data in IPC). What remains for IPC is all small: edit protocol calls, input events, overlay descriptions, metadata queries. This is why one IPC mechanism (shared memory ring buffers) works for everything — the use cases that would break a simple mechanism are handled by memory mapping instead.

### IPC ring buffers are an RPC transport (2026-03-07)

The ring buffer between editor ↔ OS service is essentially remote procedure calls. `msg_type` is a function name, payload is arguments. OS service → editor: `deliverKeyPress(keycode, modifiers, codepoint)`, `deliverMouseMove(x, y)`. Editor → OS service: `beginOperation(document, description)`, `endOperation(document)`, overlay updates. This framing means the IPC message types ARE the editor plugin API — designing one designs the other.

### Metadata queries are a separate concern from editor IPC (2026-03-07)

The editor ↔ OS service channel carries real-time control messages: input events, edit protocol (beginOp/endOp), overlays. Metadata queries (search by tags, find documents by attribute) are request/response, potentially large results, not real-time — a fundamentally different interaction pattern. They're primarily a shell/GUI concern, not an editor concern. Mixing them into the same ring buffer conflates two different cadences. Separate interface, design later.

### Scheduling contexts are the policy/mechanism boundary (2026-03-08)

Scheduling is both policy and mechanism, and the two are separable. Mechanism (context switching, timer interrupts, register save/restore) and algorithm (EEVDF selection, budget enforcement) must live in the kernel — they require EL1 privileges and run on the critical path (250Hz × 4 cores = 1,000 decisions/sec). Policy (which threads deserve what budgets, when to adjust) belongs in the OS service — it has the semantic knowledge (content types, document state, user focus). Scheduling contexts are the interface between the two layers: the kernel says "I enforce whatever budget you give me," the OS service says "this editor needs 1ms/5ms because it's playing audio." Moving the algorithm to userspace would require an IPC round-trip on every timer tick — untenable. This is the same separation Linux uses (kernel EEVDF + cgroup budgets), arrived at independently from first principles.

### A webpage is a compound document (2026-03-08)

The OS's compound document model (manifests + referenced content + layout model) maps structurally to web content. HTML is the manifest with layout rules. CSS provides layout (flow, grid, fixed positioning — covering 4 of 5 fundamental layouts natively). Images, video, and fonts are referenced content. This structural equivalence means web content could be handled through the same translator pattern as .docx or .pptx — translated into the internal compound document representation at the boundary. "Browsing" becomes "viewing HTML documents through the same rendering path as any other compound document." The rendering direction (web engine renders everything vs. native renderer with web-to-compound-doc translation) is an open sub-question, but the structural mapping holds regardless.

### Rendering and drivers face opposite constraints (2026-03-08)

The "rethink everything" stance (Decision #3) helps with drivers and hurts with rendering. Drivers need narrow scope (just your hardware), each is a bounded problem, and first-principles design is an advantage. Rendering needs broad scope (reasonable coverage of common web features — you'd notice gaps in normal browsing), can't be built from scratch (web engines are millions of lines of code), and must accommodate external reality. The adaptation layer (foundations.md) resolves this asymmetry: push engine complexity into the blue layer, keep the OS core clean. This is exactly the kind of external/internal tension the adaptation layer was designed for. The driver model can be explored through building a small set of real drivers; the rendering model must be explored through integration with an existing engine.

### Native renderer preserves the direction of power (2026-03-08)

With a web engine as renderer (Approach A), the OS can only do what the engine supports. Custom rendering behavior means patching the engine or hoping for extension points — the OS is downstream of someone else's architectural decisions. With a native renderer (Approach B), the OS defines what's possible. The renderer can express layout behaviors, compositing effects, and content-type-specific rendering that CSS can't describe. Web content is a lossy import (translated inward to compound doc format, same as .docx), not the rendering model itself. The Safari analogy: Apple controls WebKit _and_ the platform, so they can add proprietary CSS extensions — but they're still constrained by the engine's architecture. A native renderer removes that constraint entirely. The compound document model is the internal truth; external formats (.docx, .pptx, .html) are all translations inward at the boundary. The OS doesn't think in HTML any more than it thinks in .docx.

### Settling the approach, not the technology (2026-03-08)

Decision #11 was settled by choosing the architectural approach (web engine as substrate, adaptation layer between engine and OS service) without committing to a specific engine. The interesting design work is in the interface between engine and OS service — the "blue layer" — not in the engine choice itself. The engine is a leaf node: complex inside, simple interface. Any engine that can be adapted to speak the OS's protocol works. This mirrors how Decision #16 settled IPC (shared memory ring buffers) without specifying message formats. The pattern: settle the architecture, defer the implementation.

### Files are a feature, not a limitation (2026-03-08)

Phantom OS tried to eliminate files entirely via orthogonal persistence (memory IS storage). The problems it encountered — ratchet (bugs persist forever, no clean restart), schema evolution (code updates vs persistent object structures), blast radius (one corrupted object graph poisons everything), GC at scale (unsolved) — are all consequences of removing the boundaries that files provide. Files give you: isolation (corrupt one document, not the system), format boundaries (schema evolution via format versioning), natural undo points (COW snapshots per file), and interoperability (external formats). Our "no save" approach preserves the same UX ("I never lose work") by writing immediately to a COW filesystem — getting the benefit without the systemic fragility. The lesson: the boundary between "document" and "storage" is load-bearing, not incidental.

### BeOS independently validated three of our decisions (2026-03-08)

BeOS/Haiku has been running with: MIME as OS-managed filesystem metadata (our Decision #5), typed indexed queryable attributes replacing folder navigation (our Decision #7), and a system-level Translation Kit with interchange formats (our Decision #14) — for 25+ years. We arrived at the same designs from first principles. This is strong validation. The differences that matter: BeOS attributes are lost on non-BFS volumes (portability problem), BFS indexes aren't retroactive (our system should be), translators don't chain automatically (open question for us), and BeOS is still app-centric at runtime (our OS-owns-rendering model is more radical).

### Typed IPC contracts formalize the edit protocol (2026-03-08)

Singularity's channel contracts are state machines defining valid message sequences with typed payloads. Compiler proves endpoints agree on protocol state. Our edit protocol (beginOp/endOp) is already a state machine. Formalizing IPC messages as contracts — even without compiler enforcement — would prevent editors from deadlocking the OS service, document the editor plugin API precisely (since "IPC message types ARE the editor plugin API"), and enable runtime validation at the trust boundary. This should inform the IPC message format design when we get there.

### Oberon's text-as-command eliminates the CLI/GUI distinction (2026-03-08)

In Oberon, any text on screen is potentially a command. Middle-click on `Module.Procedure` in any document and it executes. "Tool texts" are editable documents containing commands — user-configurable menus that are just text files. The insight: there IS no CLI/GUI split. Text is both content and command. Every document is simultaneously a workspace. This directly addresses our open thread on CLI/GUI parity (Decision #17). Our content-type awareness could recognize "command references" within text — a tool text becomes a compound document where some content is executable.

### The kernel is a handle multiplexer with one wait primitive (2026-03-08)

A pattern emerged from settling drivers and filesystem: the kernel's job is multiplexing hardware resources behind handles + providing a single event-driven wait mechanism (`wait`). Memory (address spaces), communication (channels), time (scheduling contexts), devices (MMIO mappings + interrupt handles), timers — all accessed via handles, all waited on via one syscall. The kernel doesn't understand what any of these are _for_. It just manages them. This is a concrete identity statement for the kernel: it's the handle multiplexer. Everything semantic (content types, document state, filesystem layout, driver protocols, rendering) lives in userspace. The consequence: every new kernel feature should be expressible as "a new handle type that can be waited on." See also "Syscall API: composable verbs on typed handles" for the full API shape.

### Syscall API: composable verbs on typed handles (2026-03-08)

The syscall surface should be a small set of composable verbs, not per-type specialized calls. Three families emerged from the design discussion:

**Handle family (generic verbs, any handle type):** `wait(handles[])` blocks until any handle is ready (multiplexer — subsumes the old `channel_wait`). `close(handle)` releases any handle. `signal(handle)` notifies a channel peer. New handle types (timers, interrupts) get `wait` support for free — "every new kernel feature should be expressible as a new handle type that can be waited on."

**Synchronization family (address-based, no handles):** `futex_wait(addr, expected)` and `futex_wake(addr, count)`. Separate from handles because futexes are synchronization primitives, not event sources — you never multiplex across locks. PA-keyed for cross-process shared memory.

**Scheduling family (domain-specific verbs):** `sched_create`, `sched_bind`, `sched_borrow`, `sched_return`. Prefixed because `borrow`/`return` are too generic alone, and these operations are genuinely type-specific.

Design principles: (1) handle type carries context — `signal(channel_handle)` not `channel_signal`; (2) `wait` takes multiple handles because multiplexing IS its purpose — other syscalls take single handles; (3) streams/reactive composition lives in the OS service (userspace), not the kernel — the kernel provides the event primitive (`wait`), the OS service composes it.

The OS service architecture is naturally reactive/stream-based: merge input events, edit protocol events, and timer ticks → fold into document state → render. This maps cleanly to reactive stream combinators (most.js, RxJS pattern). The kernel doesn't need to understand streams — it just needs to be a good event source.

### Virtual manifests: documents as interfaces, not necessarily files on disk (2026-03-09)

A manifest can be static (stored on disk, COW-snapshotted) or virtual (content generated by the OS service on read, like Plan 9's `/proc`). Static manifests back user-created content. Virtual manifests back system-derived views: inbox (query over messages), search results, "recent documents," system dashboard. Both are files in the filesystem namespace. Both are documents to the user. The distinction is an implementation detail — same interface, different backing.

Virtual documents don't need their own COW history. Their "state at time T" is recoverable by re-evaluating the query against the snapshot of the world at time T. The underlying static documents have COW history; virtual documents inherit time-travel for free. Same reason database views don't need their own transaction log.

Key analogy: a video file is static on disk, but the user sees content that changes over time (temporal axis). An inbox is computed from live state, and the user sees content that changes as messages arrive. From the user's perspective, both are "things that show changing content." The mechanism differs; the experience doesn't. Virtual vs static is like table vs view in a database.

### All documents are persistent — "transient" is a retention policy, not a concept (2026-03-09)

Initially proposed "transient documents" (in-memory only, discarded on close) for things like viewed webpages. But this creates two persistence types the user must understand — a leaky abstraction. Instead: all documents are persistent by default. Webpages, imports, everything is written to the COW filesystem. Retention policies handle cleanup — viewed webpages might be kept for 30 days, user-created content kept permanently. The COW pruning system (needed anyway for edit history) handles document lifecycle too. One mechanism, not two.

This gives significant benefits for free: rewindable browsing (COW history of page views), offline access (previously viewed pages are on disk), full-text search across browsed content. Browsers already cache page assets to disk — this model structures that same data as first-class documents instead of an opaque cache blob.

Streaming content (YouTube video) is a virtual document: the manifest is persistent (metadata about what you're watching), but content is generated on demand from an external source. Same pattern as inbox (generated from internal state) — virtual manifests can derive content from internal OR external sources.

### Document mimetype resolution: imports vs OS-native (2026-03-09)

Imported documents retain their original external mimetype as manifest metadata (e.g., `application/vnd.openxmlformats-officedocument.presentationml.presentation` for .pptx). OS-native documents get custom mimetypes (e.g., `application/x-os-presentation`). The document-level mimetype drives editor binding. On export, the user selects a target format; the OS pre-selects the original mimetype where available (re-export imported .pptx defaults to .pptx). For OS-native documents, the user chooses from available export translators (like png vs jpg vs webp for images). Original mimetype is an optional metadata field — present for imports, absent for OS-native. This partially resolves the "mimetype of the whole" open question.

### Uniform rewind performance is a design constraint (2026-03-09)

If virtual document rewind is noticeably slower than static document rewind, users must know whether a document is static or virtual to set expectations — the abstraction leaks. This makes the metadata DB's placement a non-negotiable: it must live on the COW filesystem so its historical state is preserved in snapshots. Querying "inbox last Tuesday" then reads from the metadata DB at Tuesday's snapshot — same cost as a current query. This constraint flows from the virtual manifest model down into the filesystem COW design (Decision #16).

### Three-axis layout model unifies compositional and organizational documents (2026-03-09)

The original five layout types (flow, fixed canvas, timeline, grid, freeform canvas) were four spatial sub-types plus one temporal sub-type. They covered compositional documents (slides, articles, video projects) but not organizational ones (source code projects, albums, playlists). The missing piece: the **logical** axis (hierarchical, sequential, flat, graph). Adding it as a third composable axis alongside spatial and temporal unifies all compound documents under one model. Every document is a point in a three-dimensional space (spatial × temporal × logical). Most use one or two axes. The model was stress-tested against spreadsheets, chat threads, musical scores, comics, mind maps, calendars, and dashboards — everything fits. No convincing fourth axis was found. Spatial, temporal, and logical correspond to the fundamental ways humans organize anything: where, when, and how-related.

### Compositor is a React render pipeline (2026-03-10)

The compositor maps 1:1 to React's architecture. Component tree = document manifest (surface tree). Virtual DOM = scene graph. Reconciliation/diff = damage calculation. Minimal DOM patches = minimal pixel updates. Render = pure function of state. Even "commit phase" is the same term. This isn't a loose analogy — it's structural identity. Both solve the same problem: given a tree of visual content that changes incrementally, efficiently update the output. The difference: React operates on semantic elements (DOM nodes), compositor operates on pixel buffers (opaque rectangles). But the orchestration pattern — declarative tree → diff → minimal update — is identical.

### Structural constraints beat clever algorithms (2026-03-10)

Traditional compositors need sophisticated occlusion culling and z-ordering because they manage 30+ arbitrary overlapping windows. Our compositor needs none of that — one-document-at-a-time + manifest-driven layout means 3-4 z-layers total. The compositor's simplicity comes from the document model (Decision #2) and interaction model (one-doc-at-a-time leaning), not from algorithmic cleverness. This is an instance of the "simple connective tissue" principle (Decision #4): structural constraints at the design level eliminate runtime complexity.

### Compound documents are nested compositing (2026-03-10)

A compound document with embedded content parts (chart in a slide, image in a text doc) creates a surface tree structurally identical to nested windows — minus chrome, minus user-driven positioning. The compositor must handle this tree. This means "no windows" doesn't mean "flat compositor" — it means "compositor shaped by document structure instead of user window management." The compositor is the mechanism that makes compound document rendering work. This connects the unresolved compound editing tension to a concrete architectural requirement.

### Uniform manifest model eliminates the simple/compound distinction (2026-03-09)

Every document is backed by a manifest — even "simple" ones (single text file). The simple/compound distinction becomes an internal property (how many content references) rather than a user-facing concept. Users see documents, never files. Manifests are the only thing the metadata query system needs to index. Content files are the source of truth for content (indexed separately for full-text search). This makes concrete the principle already stated in CLAUDE.md: "Everything-is-files is architectural, not UX. Users see abstractions, not files."

### Content-type registration via metadata eliminates a separate registry (2026-03-09)

Editors are files too. If their metadata includes which content types they handle, then the metadata query system IS the content-type registry. One system for "find me things by their properties," whether those things are documents or tools. No separate mutable registry that can get out of sync.

### Version history is orthogonal to the layout model (2026-03-09)

COW snapshots are an OS-level mechanism, not a layout axis. An audio file has content temporality (the waveform) AND version history (the edits). Conflating them would mean "this audio track starts at 0:30" and "this file was edited yesterday" live on the same axis. They don't. Content temporality is part of what the document IS. Version history is how the document has CHANGED. The COW/undo system operates on a dimension outside the layout model entirely — which is why undo is an OS feature, not an editor feature.

### Scheduling policy needs no separate interface (2026-03-09)

The OS service already knows mimetype (fundamental metadata), editor lifecycle (manages it), and document state (renders it). When an editor sends "play" through the edit protocol, the OS service both starts rendering frames AND adjusts the scheduling context via existing kernel syscalls. Content-type-aware scheduling is internal policy logic driven by information already flowing through the edit protocol. No dedicated scheduling interface needed.

### The kernel boundary has exactly two clients (2026-03-09)

Editors don't talk to the kernel directly — they talk to the OS service via IPC (channels underneath, but the editor's interface is the edit protocol). Users don't touch the kernel. The syscall API serves exactly two kinds of clients: the OS service and userspace drivers.

### Red/blue/black is a complexity principle, not an architecture diagram (2026-03-09)

The red/blue/black model (external reality → adapters → core) serves as a complexity management principle: total complexity is conserved, blue absorbs external messiness, black stays clean. The architecture has additional structure within "black" — the kernel (clean through semantic ignorance, mechanism only) and the OS service (clean through design, policy through principled interfaces). These are two different kinds of cleanness. The architecture diagram (architecture.mermaid) captures this structural detail; the red/blue/black model stays as a principle.

### OS service interfaces are where the personality lives (2026-03-09)

Interface map by boundary:

| Boundary                   | Interface                                                     | Clients             | Status             |
| -------------------------- | ------------------------------------------------------------- | ------------------- | ------------------ |
| Kernel ↔ userland          | Syscall API (24 syscalls, typed handles)                      | OS service, drivers | Mostly designed    |
| OS service ↔ Editors       | Edit protocol (beginOp/endOp, state, input)                   | Editors             | Partially designed |
| OS service ↔ Shell         | Shell interface (navigation, document lifecycle, queries)     | Shell               | Partially scoped   |
| OS service ↔ Editors/Shell | Metadata query API (document discovery)                       | Editors, shell      | Sketched (#7)      |
| Blue ↔ Black               | Translator interface (format conversion, includes web engine) | All translators     | Blank              |
| Blue ↔ Black               | Driver interface (device access)                              | Device drivers      | Sketched           |
| OS service internal        | Renderer, layout engine, compositor, scheduling policy        | —                   | Blank              |

The kernel surface is small and stable. The blue-layer interfaces are about pluggability. The OS service boundary — edit protocol, metadata queries, interaction model — defines what it feels like to use this OS. The web engine adapter is not a separate interface from the translator interface (a webpage IS a compound document, handled through the same translator pattern as .docx).

### Full-codebase review resolved: cross-team API changes are the coordination cost (2026-03-10)

Resolved all 41 issues from DESIGN.md §11 using a 4-agent team partitioned by file ownership (assembly/linker/userspace, tests, scheduler/thread, remaining kernel src). The zero-overlap rule prevented all merge conflicts. The only coordination cost was cross-boundary API changes: when one agent changed a return type (`shared_info` → `Option`, `DrainHandles` tuple order, `KillInfo` → nested `HandleCategories`), callers in other agents' files broke. Three such ripples required lead intervention. Lesson for future multi-agent work: partition by API dependency boundary, not just file ownership. The borrow checker caught a real issue in the extracted `release_thread_context_ids` helper (split borrow needed for `s.cores[core].current` vs `s.scheduling_contexts`).

### Framebuffer is an implementation detail, surfaces are the abstraction (2026-03-09)

A raw framebuffer (`map() → &mut [u8]`) is specific to software rendering. With GPU acceleration, the CPU never touches pixel data — it submits commands and the GPU writes to VRAM. `map()` doesn't even make sense when the buffer isn't in CPU-accessible memory. The real abstraction is surfaces and operations on them: create, destroy, fill, blit, present. Every real display stack converged here (Wayland's `wl_surface`, macOS's `CALayer`, Windows' `DirectComposition`). A software implementation (surfaces as RAM buffers, CPU loops for operations, virtio-gpu for present) and a GPU implementation (surfaces as VRAM textures, GPU commands for operations, page flip for present) implement the same interface — the compositor doesn't know which is behind it.

Display (get pixels to screen) and rendering (fill the buffer) are separate concerns that always happen sequentially. GPU acceleration changes who does the rendering (GPU vs CPU), not the display path. Both live in the same device and same driver because modern GPU chips have a rendering engine and a display controller on one die. This parallels the Linux DRM/KMS split: KMS handles display (mode setting, scanout), OpenGL/Vulkan handle rendering (drawing commands). Two concerns, one driver.

### Birth time is the key insight for efficient snapshots (2026-03-08)

ZFS's single most important design choice for snapshots: store the birth transaction group (TXG) in every block pointer. When freeing a block, compare its birth time to the previous snapshot's TXG — if born after, free it; if born before, it belongs to the snapshot. This gives O(1) snapshot creation, O(delta) deletion, and unlimited snapshots. The alternative (per-snapshot bitmaps) is O(N) per snapshot and limits snapshot count. RedoxFS stores only a seahash checksum in block pointers — no temporal information. Adding birth generation to block pointers would be the minimum viable change to enable proper snapshots. Dead lists (ZFS's sub-listed approach) make deletion near-optimal: O(sublists + blocks to free). For our "no save" model where `endOperation` creates a snapshot, efficient deletion is critical.

### Operation boundaries map naturally to COW transaction boundaries (2026-03-08)

`beginOperation` opens a COW transaction, editor writes are COW'd, `endOperation` commits the transaction and creates a snapshot. No impedance mismatch. The edit protocol and the filesystem protocol are structurally the same thing — this is the kind of accidental alignment that suggests the design is coherent.

### Unsafe minimization as stated invariant (2026-03-08)

Audit of all ~99 `unsafe` blocks in the kernel found zero unnecessary uses. All fall into 7 categories: inline assembly, volatile MMIO, linker symbols, page table walks, GlobalAlloc, Send/Sync impls, stack/context allocation. The kernel already follows the Asterinas pattern (unsafe foundation + safe services) emergently. Formalized as section 7.1 in kernel DESIGN.md to prevent drift as the codebase grows. Key rule: if the OS service (EL0) ever needs `unsafe`, the kernel API is missing an abstraction.

### Microkernel by convergence, not ideology (2026-03-08)

Each kernel sub-decision independently pushed complexity outward: drivers to userspace (fault isolation + unsafe minimization), filesystem to userspace (complex code outside TCB, hot path in kernel VM anyway), rendering to the OS service (not in-kernel), editors to separate processes (untrusted). What remains is exactly the microkernel set: address spaces, threads, IPC, scheduling, interrupt forwarding, handles. This wasn't a top-down decision to "build a microkernel" — it's what fell out of applying the project's principles (simple connective tissue, unsafe minimization, fault isolation, one model not two) to each sub-decision in turn. The kernel's identity emerged from its constraints: it multiplexes hardware resources behind handles and provides a single event-driven wait mechanism. Everything semantic lives in userspace. The L4 cautionary tale ("total complexity conserved") still applies — but the complexity displacement is justified at each boundary by specific architectural arguments, not by microkernel ideology.

### Trust and complexity are orthogonal axes (2026-03-10)

Red/blue/black (complexity: where does messiness live?) and kernel/OS service/tools (trust: what happens if it crashes?) are independently useful models. Conflating them creates apparent paradoxes — "where do editors go?" — because editors are messy (blue) but untrusted (not black), and those seem to point in different directions. Separating the axes reveals the architecture's symmetry: the core is both clean and trusted, adapters are both messy and untrusted, but for different reasons. The kernel is clean through ignorance. The OS service is clean through design. Drivers are messy because hardware is messy. Editors are messy because users are unpredictable.

### The blue layer wraps the core on all sides (2026-03-10)

The adaptation layer isn't just below (hardware drivers). The user is external reality too — unpredictable, shaped by expectations from other systems. Editors are "user drivers": they adapt human intent into the structured edit protocol, just as display drivers adapt device registers into the surface trait. `beginOperation/endOperation` is to editors what `create_surface/fill_rect/present` is to drivers. The OS core sits in the middle, semantically ignorant in both directions. This completes a symmetry: below (drivers adapt hardware), sides (translators adapt formats), above (editors and shell adapt users).

### The shell is a tool, not part of the OS (2026-03-10)

The shell (GUI/CLI) is architecturally identical to editors — an untrusted EL0 process in the blue layer. It binds to "system state" the same way a text editor binds to `text/*`. It translates navigational intent (find, open, switch) into OS service operations (metadata queries, document lifecycle). The OS service doesn't know or care what the interaction _feels like_ — the shell owns the UX, the OS owns the mechanism. If the shell crashes, the OS service provides a recovery fallback (same pattern as rendering a document with no editor attached). The shell is pluggable, though the OS will be tuned toward its primary shell's needs.

### User input always goes to a tool (2026-03-10)

There is always an active tool. The OS service routes input; it never interprets it. When an editor is active, it receives modification input. When no editor is active, the shell receives navigational input. This extends the editor model (one active per document) to the system level: one active tool, period. The OS service has no "bare" input handling mode. This makes the interaction model a shell design question, not an OS service design question — same separation as everywhere else (OS provides mechanism, tools bring semantics).

### Configuration is a protocol's opening sequence (2026-03-10)

Init passes device addresses and framebuffer info to drivers before starting them — fundamentally different from ongoing conversation. Initially leaned toward two mechanisms (config structs vs ring buffers). But Singularity showed the cleaner model: configuration is the opening messages in the channel's protocol. A GPU driver's "contract" starts with `state Init { receive ConfigMsg → Running }`. One mechanism, config is just the first message(s). Avoids the blurry boundary problem — what happens when a "config" channel later needs runtime updates? With one mechanism, it just sends more messages. No mechanism switch. Prior art: Singularity (contracts), QNX (MsgSend for everything). Counter-examples: Fuchsia (separate processargs), Unix (argv vs pipes). The temporal asymmetry (config is pre-start) is real but doesn't require a separate mechanism — the ring buffer is initialized before the child starts, just like the raw byte layout was.

### Fixed-size ring entries are the high-performance consensus (2026-03-10)

io_uring (64-byte SQE), LMAX Disruptor, L4 message registers, virtio descriptors (16 bytes) — all chose fixed-size entries in the ring, with variable-size data elsewhere. The arguments compound: no fragmentation, no wraparound complexity, predictable prefetching, one-cache-line-per-message on AArch64 (64 bytes = cache line). When you need large data, it goes in shared memory with a reference through the ring. This matches the OS design's existing principle (documents are memory-mapped, ring buffers carry control only) and makes it a design rule rather than a pressure point.

### Security as a side effect of good architecture (2026-03-07)

Handles enforce access (designed for edit protocol, not security). EL0/EL1 provides crash isolation (designed for clean programming model). Per-process address spaces provide memory isolation (designed for independent editors). Kernel message validation protects the OS service (designed for input correctness). Every security property falls out of design decisions made for other reasons. No security-specific machinery is needed because the architecture is naturally secure. This suggests a useful heuristic: if you're adding security features that don't serve the design, the architecture may be wrong.

### Editors as read-only consumers: "never make the wrong path the happy path" (2026-03-11)

The original edit protocol had editors calling beginOp/endOp around direct memory-mapped writes. This makes undo opt-in — a lazy editor that just writes without calling begin/end gets no undo points. The wrong path (skip the protocol) is the easy path. Inverted: editors get read-only mappings of documents. All writes go through the OS service via IPC. The OS service is the sole writer to document files, giving it full control over snapshots and undo with zero cooperation required from editors. Undo is automatic and non-circumventable. The lazy editor path (just send write requests) produces correct undo behavior. The diligent editor path (grouping writes into named operations) produces better undo granularity. The symmetry with the kernel is preserved: the kernel doesn't let processes write to other processes' memory (it goes through IPC). The OS doesn't let editors write to the OS's documents (it goes through IPC). Documents are shared resources — the OS renders them, versions them, indexes them — so mediated access follows the same principle as any shared resource. Performance is not a concern: the hot-path workloads (text editing, image adjustments) are low-bandwidth IPC; bulk data operations (rendering, audio capture) don't write to the document at those rates.

### OS service event loop eliminates file locking (2026-03-11)

Multiple data sources (editors, network services, audio drivers) may want to write to the same document concurrently (e.g., chat where user types and remote messages arrive). But all writes arrive at the OS service's event loop as IPC messages and are processed sequentially. Multiple sources of data arriving concurrently is not the same as multiple writers to a file. The serialization happens at the event loop, which already exists. No file locking needed because there's only one writer. This is the web server pattern: multiple clients, one server, sequential processing per resource.

### Copy semantics + COW = logical independence with physical efficiency (2026-03-11)

Compound documents use copy semantics — embedding a photo in a slide deck creates an independent copy. No reference tracking, no broken links, no cascading deletes. COW at the filesystem level means the "copy" shares physical blocks with the original until one diverges. The user gets clean mental model (each document is self-contained), the disk gets efficient storage (shared blocks). Original file ID stored as provenance metadata enables explicit "update to latest" (pull, not push). One-directional knowledge: compound knows about original, original doesn't know about compound.

### The filesystem is a dumb file store (2026-03-11)

By settling "OS service is the sole writer" and "compound documents use copy semantics," the filesystem's job became radically simple: store files by ID, provide memory-mapped access, take snapshots, restore snapshots. 12 operations. No paths, no permissions, no locking, no links, no file types, no extended attributes, no batch operations. Everything "interesting" (documents, undo ordering, metadata, queries, compound structures) lives above the filesystem in the OS service and metadata DB layers. Three-layer translation: user intent → metadata DB → document IDs → OS service → file IDs → filesystem. Each layer ignorant of the one above. The filesystem is the simplest possible foundation — its only job is correctness about COW, snapshots, and crash consistency.

### Compound document atomicity solved by sole-writer architecture (2026-03-11)

An edit on a compound document might touch multiple files (manifest + content files). With the OS service as sole writer, it simply does both writes sequentially and then takes a snapshot. No multi-file transaction mechanism needed in the filesystem. The atomicity problem that seemed to require filesystem support was actually solved by an architectural decision at a different layer. This is a recurring pattern: structural constraints at one level eliminate complexity at another.

---

## Research Spikes

Active or planned coding explorations. These are learning exercises, not commitments. Code may be thrown away.

### Bare metal boot on arm64 (QEMU)

**Status:** Complete — all 7 steps done
**Goal:** Build a minimal kernel on aarch64/QEMU. Learn what's involved in boot, exception handling, context switching, memory management.
**Informs:** Decision #16 (Technical Foundation) — whether writing our own kernel is tractable and worthwhile vs. building on existing.
**What exists:** `system/kernel/` — ~2,150 lines across 18 source files (at time of spike completion). boot.S (boot trampoline, coarse 2MB page tables, EL2→EL1 drop, early exception vectors), exception.S (upper-VA vectors, context save/restore, SVC routing), main.rs (Context struct, kernel_main, irq/svc dispatch, ELF loader + user thread spawn), elf.rs (pure functional ELF64 parser), build.rs (compiles user ELFs at build time), memory.rs (TTBR1 L3 refinement for W^X, PA/VA conversion, empty TTBR0 for kernel threads), heap.rs (bump allocator, 16 MiB), page_alloc.rs (free-list 4KB frame allocator), asid.rs (8-bit ASID allocator), addr_space.rs (per-process TTBR0 page tables, 4-level walk_or_create, W^X user page attrs with nG), scheduler.rs (round-robin preemptive, TTBR0 swap on context switch), thread.rs (kernel + user thread creation, separate kernel/user stacks), syscall.rs (exit/write/yield, user VA validation), timer.rs (ARM generic timer at 10 Hz), gic.rs (GICv2 driver), uart.rs (PL011 TX), mmio.rs (volatile helpers). Init later promoted to proto-OS-service at `system/services/init/`. Builds with `cargo run --release` targeting `aarch64-unknown-none` on nightly Rust.
**Original success criteria:** ~~Something boots and prints to serial console.~~ Done.
**Next steps (in order):**

1. ~~**Timer interrupt**~~ — Done. ARM generic timer fires at 10 Hz, IRQ path exercises full context save/restore, tick count prints to UART.
2. ~~**Page tables + enable MMU**~~ — Done. Identity-mapped L0→L1→L2 hierarchy with 2MB blocks, L3 4KB pages for kernel region with W^X permissions (.text RX, .rodata RO, .data/.bss/.stack RW NX).
3. ~~**Heap allocator**~~ — Done. Bump allocator (advance pointer, never free), 16 MiB starting at `__kernel_end`. Lock-free CAS loop. Unlocks `alloc` crate (Vec, Box, etc.).
4. ~~**Kernel threads + scheduler**~~ — Done. Thread struct with Context at offset 0 (compile-time assertion). Round-robin in `irq_handler` on each timer tick. Boot thread becomes idle thread (`wfe`). Box<Thread> for pointer stability (TPIDR_EL1 holds raw pointers into contexts). IRQ masking around scheduler state mutations.
5. ~~**Syscall interface**~~ — Done. SVC handler with ESR check, syscall table (exit/write/yield), user VA validation. EL0 test stub proves full EL0→SVC→EL1→eret path.
6. ~~**Per-process address spaces**~~ — Done. Kernel at upper VA (TTBR1), per-process TTBR0 with 8-bit ASID, 4-level page tables (walk_or_create), W^X user pages with nG bit, frame allocator for dynamic page table allocation, scheduler swaps TTBR0 on context switch, empty TTBR0 for kernel threads.
7. ~~**First real userspace process**~~ — Done. Standalone init binary compiled to ELF64 by build.rs, embedded in kernel via `include_bytes!`. Pure functional ELF parser extracts PT_LOAD segments. Loader allocates frames, copies data, maps with W^X permissions. Entry point from ELF header. Init later promoted to proto-OS-service at `system/services/init/`.

**Known simplifications (intentional, revisit later):** Single-core only (multi-core after userspace works). Bump allocator never frees (replace when threads are created/destroyed). No per-CPU IRQ stack (not needed — EL0→EL1 transitions use SP_EL1 automatically). 10 Hz timer (increase when scheduling granularity matters). No ASID recycling (255 max user address spaces). Coarse TTBR0 identity map from boot.S still loaded but unused after transition to upper VA.

Dependencies: All 7 steps complete. The spike validated the full stack: boot → MMU → heap → threads → syscalls → per-process address spaces → ELF loading. From-scratch kernel in Rust on aarch64 is tractable. Binary format settled as ELF.

**Risk:** If we decide to build on an existing kernel, this code is throwaway. That's fine — the knowledge isn't throwaway.
