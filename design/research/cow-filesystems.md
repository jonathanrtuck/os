# COW Filesystem Research

Research into copy-on-write filesystem designs, conducted to inform our filesystem decision (Decision #16, open sub-decision). Our OS requires COW for OS-level undo (Decision #12: snapshots at operation boundaries).

---

## Our Requirements

What our design demands from a filesystem:

1. **Frequent snapshots.** Every `endOperation` creates a snapshot. Could be dozens per minute during active editing. Snapshot creation must be O(1) or near-O(1).
2. **Efficient snapshot deletion/pruning.** Old snapshots must be reclaimable without full tree walks. Automatic pruning policy (keep last N, thin out older ones).
3. **Per-document granularity.** Undo is per-document. Whole-filesystem snapshots are wasteful if only one document changed. Need either per-subtree snapshots or a way to scope snapshot cost to changed data.
4. **Content-type metadata.** Mimetype is OS-managed metadata on every file. Must be first-class, not an afterthought (xattrs are fine if fast).
5. **COW for immediate writes.** Editors write immediately (no "save"). COW ensures the previous state is always recoverable. No write-ahead log needed — the old blocks ARE the log.
6. **Memory-mapped file support.** Documents are memory-mapped into editor and OS service address spaces. The filesystem must support this efficiently (page-aligned blocks, coherent mapping).

---

## RedoxFS

**Source:** [gitlab.redox-os.org/redox-os/redoxfs](https://gitlab.redox-os.org/redox-os/redoxfs), ~6K lines Rust, MIT licensed.

### On-Disk Format

- **Block size:** Fixed 4 KiB.
- **Header ring:** First 256 blocks are a rotating ring of superblocks. Each commit writes to the next slot with an incremented generation number. On mount, scan for highest valid generation. Provides crash recovery (fall back to previous generation).
- **Node:** One block per inode. Unix metadata (mode, uid/gid, size, timestamps). 5-level indirect block pointer tree for file data:
  - L0: 128 direct pointers = 16 MiB
  - L1: 64 single-indirect = 2 GiB
  - L2: 32 double-indirect = 256 GiB
  - L3: 16 triple-indirect = 32 TiB
  - L4: 8 quad-indirect = 4 PiB (theoretical max ~193 TiB)
- **Block pointers:** 16 bytes each — block address (u64, encodes index + level + compression) + seahash checksum (u64). Parent validates child via checksum (Merkle-like).
- **Namespace tree:** 4-level fixed-depth radix trie (not a B-tree). 32-bit node IDs decomposed into 8-bit chunks. Lookup is always exactly 4 block reads. ~4.3 billion max files.
- **Directories:** Variable-length entries packed in blocks, linear scan for lookup.
- **Small files:** Inline data flag stores content directly in the node's level_data field.

### COW Mechanism

- **Transaction model:** All writes buffered in a `BTreeMap` write cache. Flushed atomically on `sync()`.
- **Write path:** Allocate new block address -> swap pointer from old to new -> defer old block deallocation -> cache data.
- **Two-phase deallocation:** Blocks allocated in the current transaction can be immediately reclaimed on rollback. Blocks from previous transactions are queued for deferred deallocation — can't be reused until after the new header is committed. Prevents crash corruption.
- **Commit point:** Only the header write is the commit point. Crash before = old generation valid (new blocks orphaned, recovered at GC). Crash after = new generation canonical.
- **Allocator:** Buddy allocator (in-memory `BTreeSet` per level). On-disk persistence via `AllocList` chain. GC compacts the allocator log every 1024 generations.

### Snapshot Support: NOT IMPLEMENTED

Despite marketing materials mentioning snapshots, the source code contains no snapshot mechanism. The header ring provides crash recovery (up to 256 previous generations), but:

- No way to pin a generation
- No snapshot naming or management API
- Old blocks are freed by GC — not preserved for snapshots
- No birth-time in block pointers (only seahash)
- No dead lists

The COW machinery is a foundation for snapshots, but the gap is substantial.

### Other Features

- **Encryption:** AES-128-XTS, full disk, 64 key slots, Argon2id key derivation.
- **Compression:** LZ4 per-block, transparent.
- **Cache:** Write-through LRU, ~16 MiB.
- **Platform:** Runs on Redox natively and Linux via FUSE.

### Assessment for Our Needs

| Requirement           | RedoxFS                       | Gap                       |
| --------------------- | ----------------------------- | ------------------------- |
| O(1) snapshots        | No snapshots                  | Critical gap              |
| Efficient pruning     | N/A                           | Critical gap              |
| Per-document scope    | No (whole-FS only)            | Design gap                |
| Content-type metadata | Standard Unix mode/times only | Minor (add field to node) |
| COW immediate writes  | Yes, solid                    | Met                       |
| Memory-mapped files   | 4 KiB aligned blocks          | Compatible                |

**Verdict:** RedoxFS is a clean, small COW filesystem, but it's a starting point for our needs, not a solution. The COW transaction machinery is solid. Everything above that (snapshots, pruning, per-document scoping) would need to be designed and built.

---

## ZFS

**Source:** OpenZFS, ~500K lines C, 20+ years battle-tested.

### Architecture

- **Merkle tree of blocks.** Every block pointer (128 bytes) stores: up to 3 DVAs (redundancy), sizes, compression type, checksum, and critically — **birth TXG** (transaction group number when the block was allocated).
- **COW from leaves to root.** Modify data -> COW indirect blocks -> COW propagates to uberblock root -> atomic uberblock write (ring of 128 per vdev label).
- **Dataset layer (DSL).** Filesystems and snapshots are "datasets" — named roots into the block tree. This layer manages snapshot relationships.

### Snapshot Mechanism (Matt Ahrens, BSDCAN 2019)

**Creation: O(1).** Save the current root block pointer. COW ensures old blocks are preserved — the saved pointer remains a valid, consistent view forever. No copying, no traversal.

**The birth-time insight:** Each block pointer records its child's birth TXG. When freeing a block, compare birth time to the previous snapshot's TXG:

- Birth > prevsnap: block was born after the snapshot, safe to free.
- Birth <= prevsnap: block is referenced by the snapshot, do NOT free. Add to dead list instead.

This gives: O(1) space per snapshot, O(1) create, O(delta) delete, **unlimited snapshots**.

Alternative approaches and why ZFS rejected them:

- **Per-snapshot bitmaps:** O(N) space per snapshot, O(N) create/delete. Limits snapshot count. Doesn't scale.
- **Reference counting (Btrfs approach):** Works but makes snapshot deletion expensive (must decrement refcounts through entire tree).

### Dead Lists (snapshot deletion optimization)

When a block can't be freed (snapshot references it), it goes on a **dead list**:

- Each dataset maintains a dead list.
- Dead lists organized into **sub-lists by birth time range** (one per earlier snapshot).

Three deletion algorithms, progressively faster:

1. **Turtle (naive):** Traverse block tree. Birth <= prevsnap? Skip subtree. Otherwise check next snap. O(blocks written since prevsnap). Slow — random reads.
2. **Rabbit (dead lists):** Traverse next snap's dead list. Free blocks with birth > prevsnap. Merge dead lists. O(deadlist size). Up to 2048x faster — sequential reads.
3. **Cheetah (sub-lists):** Iterate sub-lists by birth range. If min TXG > prevsnap, free ALL blocks in sub-list without examining them. O(sublists + blocks to free). Near-optimal.

### Space Accounting

- Snapshot's `used` = its unique blocks (freed if this snapshot deleted).
- `usedbysnapshots` = total held by all snapshots.
- `written` = how much changed since a snapshot (O(1) via dead list examination).
- Most space is shared between adjacent snapshots.

### Assessment for Our Needs

ZFS's snapshot design is the gold standard. The birth-time-in-block-pointers insight is the key enabler. Dead lists with sub-listing make deletion tractable at any scale. But ZFS is whole-filesystem snapshots — per-document scoping would need a layer on top (one dataset per document? per-document snapshot namespace?).

---

## Btrfs

**Source:** Linux kernel, ~150K lines C, 15+ years.

### Architecture

- **COW B-trees.** All metadata in B-trees that COW on modification. Keys are `(objectid, type, offset)` triples.
- **Reference counting.** Every allocated extent has a refcount. COW on a B-tree node increments refcounts of all children.
- **Subvolumes.** Named B-trees with their own inodes. Snapshots ARE subvolumes — structurally identical, created by incrementing the root block's refcount.

### Snapshot Mechanism

**Creation: O(1).** Increment refcount on root block of source subvolume. Future COW makes changes private to each root.

**Snapshots are writable** (unlike ZFS where snapshots are read-only by default). This is interesting for our "no save" model — a snapshot could be a working copy.

### Deletion

No dead lists. Relies on reference counting throughout. Deleting a snapshot decrements root's refcount; if zero, depth-first traversal frees blocks and decrements their children's refcounts recursively. Large deletions are checkpointed across transactions.

**Problem:** Cascading refcount updates can be expensive. Snapshot deletion of a deeply modified tree touches many blocks.

### Assessment for Our Needs

Btrfs's subvolume model (snapshots = writable subvolumes) maps interestingly to documents — each document could be a subvolume. But refcount-based deletion is less predictable than ZFS's dead-list approach, and refcount cascading under heavy snapshot load is a known pain point in Btrfs.

---

## Bcachefs

**Source:** Linux kernel (mainlined 6.7, Jan 2024), ~100K lines C + Rust CLI.

### Architecture: Key-Level Versioning

Fundamentally different from ZFS/Btrfs. Does NOT clone trees for snapshots. Instead:

- **Every filesystem item** (inode, dirent, xattr, extent) is extended with a **snapshot ID**.
- Snapshot IDs form a hierarchical tree. Root starts at U32_MAX, new IDs allocated downward (parent > child).
- Lookups check whether a key's snapshot ID is an ancestor of the current snapshot.
- Snapshots exist as version-annotated entries in the same B-tree — no separate tree per snapshot.
- B-tree nodes are 256 KiB (unusually large), internally log-structured.
- Updates are **journalled** (unlike pure COW), improving random write performance.

### Advantages

- No per-snapshot tree duplication (lower metadata overhead).
- Better scalability for frequent sparse snapshots.
- No extent refcounting.

### Disadvantages

- Snapshot deletion requires full B-tree walk (no dead lists, no birth-time shortcut).
- Space accounting is "the trickiest and most complicated part" (author's words).
- Fsck with snapshots is extremely complex.
- 64-subvolume simultaneous snapshot limit.
- Dense overwrites of heavily-snapshotted files fragment badly.

### Assessment for Our Needs

Key-level versioning is architecturally interesting for per-document undo. Instead of snapshotting an entire tree, you version individual file extents. This naturally scopes snapshot cost to changed data. The downsides (deletion cost, space accounting complexity) are real but might be acceptable if snapshots are pruned aggressively. Worth considering as an alternative to ZFS-style whole-tree snapshots.

---

## TFS (Redox's Deprecated Predecessor)

Abandoned, but "most features incorporated into RedoxFS." Notable features that did NOT make it into RedoxFS:

- **Revision history:** Automatic file versioning. This is exactly what we need — per-file snapshots. It was designed but apparently too complex to ship.
- **Segment-based COW:** Only modified segments copied (finer than block-level).
- **Bloom-filter GC:** Interesting approach to garbage collection.
- **SPECK cipher** (not AES) — less standard.

The fact that TFS attempted per-file revision history and didn't ship it is a cautionary data point.

---

## Synthesis: What Should We Build?

### The Core Tradeoff

| Approach                            | Snapshot scope   | Create | Delete         | Space tracking | Complexity        |
| ----------------------------------- | ---------------- | ------ | -------------- | -------------- | ----------------- |
| ZFS-style (birth time + dead lists) | Whole filesystem | O(1)   | O(delta)       | Excellent      | Medium            |
| Btrfs-style (refcounted subvolumes) | Per-subvolume    | O(1)   | O(tree walk)   | Good           | Medium            |
| Bcachefs-style (key versioning)     | Per-key          | O(1)   | O(B-tree walk) | Poor           | High              |
| Per-file revision log               | Per-file         | O(1)   | O(1)           | Direct         | Low (but limited) |

### Recommendations

**1. Birth time in block pointers is non-negotiable.**

ZFS proved this. Without temporal metadata in block pointers, you can't efficiently determine which blocks are reclaimable when deleting a snapshot. RedoxFS's block pointer has room — replace the 8-byte seahash with 4-byte seahash + 4-byte birth generation (or expand the pointer). This single change enables the entire ZFS snapshot deletion algorithm.

**2. ZFS-style dead lists are worth the complexity.**

The cheetah algorithm (sub-listed dead lists) makes snapshot deletion O(sublists + blocks to free) regardless of filesystem size. For our "no save" model where snapshots are created at every `endOperation`, efficient deletion is critical — we'll accumulate thousands of snapshots per session and need to prune aggressively.

**3. Per-document scoping via datasets or subvolumes.**

ZFS datasets and Btrfs subvolumes both provide named, independent snapshot namespaces. Each open document could be a dataset/subvolume with its own snapshot chain. This avoids whole-FS snapshots when only one document changed. The document's `endOperation` snapshots only its dataset.

**4. Bcachefs key-versioning as an alternative worth prototyping.**

If per-document datasets feel too heavyweight, bcachefs's approach — version individual extents with snapshot IDs — naturally scopes cost to changed data without requiring separate trees. The deletion cost (full B-tree walk) could be acceptable if we maintain a per-document index of affected keys.

**5. The header ring pattern (from RedoxFS) is good for crash recovery.**

Even with proper snapshots, a rotating ring of superblocks provides an independent crash-recovery mechanism. Keep it.

**6. Operation boundaries = transaction boundaries.**

`beginOperation` opens a COW transaction. The editor writes (all COW'd). `endOperation` commits the transaction and creates a snapshot. This maps naturally — no impedance mismatch between the edit protocol and the filesystem.

### Open Questions

- **Snapshot naming/indexing:** Sequential generation numbers? Per-document operation counter? Need to support "undo 3 operations" efficiently.
- **Pruning policy:** Keep all snapshots from current session? Thin out to hourly/daily for older sessions? User-configurable retention?
- **Compound document snapshots:** A compound document is multiple files (manifest + parts). Snapshotting must be atomic across all files — either via a shared dataset or a multi-file transaction.
- **Interaction with memory mapping:** When a document is memory-mapped and the editor writes, how does the COW layer intercept the write? Trap on write (mark pages read-only, handle fault, COW, remap)? Or require all writes through a syscall? The latter conflicts with shared-memory direct access.

### Prior Art Summary

| System   | Key Innovation for Us                         | Limitation                       |
| -------- | --------------------------------------------- | -------------------------------- |
| ZFS      | Birth time + dead lists = efficient snapshots | Whole-FS scope, massive codebase |
| Btrfs    | Subvolumes as snapshot namespaces             | Refcount cascading under load    |
| Bcachefs | Key-level versioning (per-extent snapshots)   | Deletion cost, space accounting  |
| RedoxFS  | Clean Rust COW machinery, small codebase      | No snapshots implemented         |
| TFS      | Attempted per-file revision history           | Didn't ship (complexity?)        |

---

## Transaction Commit Protocols (2026-03-25)

Research into how COW filesystems achieve atomic state transitions. The core question: what is the single write that transitions old state to new state, and what guarantees does the underlying storage need to provide?

### The Universal COW Commit Pattern

Every pure COW filesystem follows the same fundamental pattern:

1. Write new data blocks to free locations (old blocks untouched)
2. Write new metadata blocks pointing to the new data (old metadata untouched)
3. **Barrier/flush** — ensure all of (1) and (2) are durable
4. Write the new root pointer (superblock/uberblock/meta page)
5. **Barrier/flush** — ensure (4) is durable before proceeding

Step 4 is the **atomic commit point**. Before it completes, the filesystem is in the old state. After it completes, the filesystem is in the new state. If power is lost during step 4, the root pointer is either fully written (new state) or torn/invalid, in which case a previous valid copy is used (old state). There is no intermediate state.

The key insight: COW makes the state transition a function of a single root pointer update. Everything else is setup (writing new blocks to unused locations) that is invisible until the root pointer changes.

---

### ZFS: Transaction Group Commit

**Source:** OpenZFS `module/zfs/vdev_label.c`, `module/zfs/txg.c`

#### Transaction Group Lifecycle

ZFS batches all mutations into **transaction groups** (txgs), each with a monotonically increasing 64-bit ID. Three states run concurrently:

1. **Open** — accepting new writes. New transactions attach to the currently open txg.
2. **Quiescing** — draining in-flight transactions. Brief (bounded by software latency, not I/O).
3. **Syncing** — writing all accumulated changes to disk. Iterative: writing data triggers metadata changes (space maps), which trigger more writes, until convergence.

Default commit interval: every 5 seconds (tunable). A txg may also close early if it reaches a size threshold.

#### The Commit Sequence (vdev_config_sync)

After the syncing state converges (all data, indirect blocks, and metadata are on disk), the root pointer must be updated. ZFS uses a **two-phase label protocol**:

1. **Write labels L0 and L2** (even-numbered) with the new nvlist configuration
2. **Flush** — barrier to ensure L0/L2 are durable
3. **Write the uberblock** to all four labels' uberblock rings
4. **Flush** — barrier to ensure uberblock is durable
5. **Write labels L1 and L3** (odd-numbered) with the new configuration
6. **Flush** — final barrier

The design comment in the source states the order is "carefully crafted to ensure that if the system panics or loses power at any time, the state on disk is still transactionally consistent."

#### Crash Recovery Reasoning

The two-phase label split provides this invariant:

- If crash after step 1 but before step 3: L0/L2 have a future txg, but the uberblock still points to the old txg. Recovery finds L1/L3 valid (they match the uberblock's txg). L0/L2 are discarded as ahead-of-uberblock.
- If crash after step 3 but before step 5: The uberblock points to the new txg. L0/L2 are valid (they match). L1/L3 are stale but harmless (the uberblock's txg > L1/L3's txg, so L0/L2 are preferred).
- If crash during step 3: The uberblock is either complete (new state) or torn. Torn detection via checksum → fall back to previous uberblock in the ring.

**Note:** A known issue (openzfs/zfs#4162) identified that `zpool_read_label()` in userspace does not compare label txg against uberblock txg during import, which can cause label mismatch errors after a crash during step 1. The kernel import path handles this correctly.

#### Uberblock Structure

Each vdev (physical device) has **4 labels**, each 256 KiB:

- Labels L0/L1 at the start of the device
- Labels L2/L3 at the end of the device
- Each label contains: 8 KiB blank + 8 KiB boot header + 112 KiB nvlist + **128 KiB uberblock ring**
- The uberblock ring holds **128 uberblocks of 1 KiB each**, written round-robin

Each uberblock contains:

- `ub_txg` — transaction group number (the monotonic generation counter)
- `ub_timestamp` — wall clock at commit time
- `ub_rootbp` — block pointer to the Meta Object Set (MOS), the root of all pool state
- Checksum covering the uberblock

**Active uberblock selection at mount:** Scan all 128 uberblocks across all 4 labels. The one with the **highest txg AND valid checksum** wins. This means up to 128 previous states are available for fallback.

#### What ZFS Requires from the Block Device

- **Sector atomicity**: A single 512-byte (or 4K) sector write must be atomic. ZFS's uberblock is 1 KiB — fits in 2 sectors on 512-byte devices or 1 sector on 4K devices.
- **Write barriers / flush**: Required between the three phases of `vdev_config_sync`. ZFS issues `zio_flush()` after each phase. Without flush, disk reordering could write the uberblock before the data it references.
- **Multiple copies**: 4 labels x 128 uberblocks = 512 uberblock copies per device. Redundancy compensates for torn writes to individual copies.

#### ZIL Ordering Subtlety

The ZFS Intent Log (ZIL) is freed during the syncing txg, but freed ZIL blocks are **not reusable for 3 txgs** (TXG_DEFER_SIZE). This means even if the uberblock update races with ZIL cleanup, crash recovery can replay ZIL entries from blocks that haven't been reallocated yet. This eliminates a strict ordering dependency between ZIL sync and uberblock update.

---

### Btrfs: Superblock Commit

**Source:** Linux kernel `fs/btrfs/transaction.c`, `fs/btrfs/disk-io.c`

#### Transaction Lifecycle

Btrfs groups mutations into numbered **generations**. Multiple filesystem operations join the currently open transaction. Commit happens periodically (default ~30 seconds) or on explicit sync.

#### The Commit Sequence (btrfs_commit_transaction)

1. **Flush delayed operations** — delayed refs, delayed inodes, ordered extents
2. **COW all dirty B-tree roots** — `commit_cowonly_roots()` iterates, writing dirty metadata blocks (extent tree, root tree, etc.) until convergence
3. **Write all dirty extents** — `btrfs_write_marked_extents()` + `btrfs_wait_extents()` ensures metadata pages reach disk
4. **Flush** — barrier to ensure all metadata is durable
5. **Write superblock** — `write_all_supers()` writes the new root tree pointer, generation number, and checksum
6. **Flush** — barrier to ensure superblock is durable

Step 5 is the atomic commit point.

#### Superblock Structure

Three copies at fixed byte offsets:

- Primary: 64 KiB (0x10000)
- Mirror 1: 64 MiB (0x4000000)
- Mirror 2: 256 GiB (0x4000000000) — only if the device is large enough

The superblock (4 KiB) contains:

- CRC32c checksum (bytes 0x00-0x1F, covers everything after)
- Filesystem UUID
- **Generation** — monotonic transaction counter
- **Root tree root** — logical address of the root tree (which then points to all other trees)
- Chunk tree root — for volume management
- Log tree root — for crash replay

At mount, Btrfs reads the primary superblock at 64 KiB. If invalid, it falls back to mirrors. The superblock with the highest valid generation wins.

#### What Btrfs Requires from the Block Device

- **Flush command**: Btrfs's consistency model explicitly states: "there's a flush command that instructs the device to forcibly order writes before and after the command." The documentation warns: "Disabling barriers...will most certainly lead to a corrupted filesystem in case of a crash or power loss."
- **Single-sector atomicity**: The superblock is 4 KiB. On a 4K-sector device, this is one sector write. On a 512-byte device, this is 8 sectors — Btrfs relies on checksums + 3 mirrors to survive torn writes.
- **Write ordering via flush**: Btrfs does NOT rely on natural ordering. It assumes the device may reorder writes unless separated by flush. Two flushes per commit: one before superblock write (data durable), one after (superblock durable).

#### Key Difference from ZFS

Btrfs has a simpler commit protocol (no two-phase label dance) but relies more heavily on working flush/barrier support. ZFS's 4-label redundancy provides more defense-in-depth against barrier failures. Btrfs's 3 superblock mirrors provide less redundancy (3 copies vs. ZFS's 512 uberblock copies across labels).

---

### RedoxFS: Header Ring Commit

**Source:** `redoxfs` Rust crate, ~6K lines

#### Transaction Model

All writes accumulate in an in-memory `BTreeMap` write cache. `sync()` flushes everything:

1. Write all dirty data blocks to their new (COW'd) locations on disk
2. Write the updated inode and indirect blocks
3. Write the new header (superblock) to the next slot in the ring

#### Header Ring

First 256 blocks (256 x 4 KiB = 1 MiB) form a rotating ring of superblock copies. Each commit increments the generation counter and writes to `ring[generation % 256]`.

At mount: scan all 256 slots, select the one with the **highest generation that passes checksum validation**.

#### Atomic Commit Point

The header write at step 3. If torn, the checksum fails, and the previous slot (generation - 1) is used instead.

#### Two-Phase Deallocation

Blocks freed in the current transaction are tracked separately from blocks freed from previous transactions. Current-transaction blocks can be immediately reclaimed on rollback. Previous-transaction blocks are deferred — they cannot be reused until after the new header is committed. This prevents a crash during commit from corrupting data referenced by the old header.

#### What RedoxFS Requires

No explicit flush/barrier calls in the source code. The implicit assumption is that the header write reaches disk after the data blocks. This is safe when:

- Running on a synchronous block device (writethrough), OR
- The host OS orders writes correctly (e.g., FUSE with proper fsync), OR
- On actual hardware, the lack of barriers is a latent crash-consistency bug

This is a gap — for a real filesystem, flush commands would be needed between data writes and header writes.

---

### WAFL (NetApp): Consistency Points

**Source:** WAFL papers, NetApp documentation

#### Architecture

WAFL (Write Anywhere File Layout) is NetApp's proprietary COW filesystem. It uses **Redirect-on-Write** (ROW): modified blocks are written to new locations, not copied first. Parent pointers are updated in memory.

#### Consistency Point (CP)

The CP is WAFL's transaction commit:

1. All dirty blocks in memory are written to new locations on disk
2. Once all blocks are durable, the **root inode** (fsinfo block) is updated

The root inode is the atomic commit point. All other blocks are reached via the root inode — until the root inode changes, none of the new blocks are visible.

Key quote from the architecture: "As all blocks, other than the block containing the root inode, are found via the root inode, none of the changes written to permanent storage are visible on permanent storage until the root inode is updated."

#### NVRAM Acceleration

WAFL uses hardware NVRAM (battery-backed RAM) to log changes. This allows it to:

- Acknowledge writes to clients before they reach disk (NVRAM is persistent across power loss)
- Replay NVRAM log entries after crash to reconstruct the latest CP
- Avoid the flush-before-root-update ordering constraint because NVRAM survives power loss

CP frequency: every 10-30 seconds typically.

#### What WAFL Requires

- NVRAM for write ordering (not applicable to our design)
- Without NVRAM: standard flush semantics (data durable before root pointer update)

**Our takeaway:** WAFL confirms the universal pattern (COW data + atomic root pointer update) but its NVRAM dependency makes its specific protocol non-transferable. The fsinfo block concept (single root pointer) is identical in principle to ZFS's uberblock and Btrfs's superblock.

---

### LMDB: Shadow Paging (Double-Buffered Meta Pages)

**Source:** LMDB (Lightning Memory-Mapped Database), Howard Chu

LMDB is not a filesystem but uses the same COW commit pattern at the database level. Worth studying because it achieves crash consistency with minimal mechanism.

#### Architecture

- B+ tree stored in a memory-mapped file
- **Two meta pages** at pages 0 and 1 (ping-pong/double-buffer)
- All data modifications are COW — new pages written, old pages preserved

#### Commit Sequence

1. Write all dirty data pages to new locations in the file
2. `fsync()` — ensure all data pages are durable
3. Write the new meta page (alternating between page 0 and page 1)
4. `fsync()` — ensure meta page is durable

The meta page contains a **transaction ID** (monotonic counter). The update of the transaction ID is claimed to be atomic because it fits in a single machine word.

#### Recovery

Read both meta pages. Select the one with the **highest valid transaction ID**. If the latest is torn (detected by checksum or invalid content), the previous meta page is still valid.

#### What LMDB Requires

- `fsync()` that actually flushes to durable storage (on macOS: `fcntl(F_FULLFSYNC)`)
- Sector-atomic write for the meta page (or at minimum, the transaction ID word)
- Write ordering: data pages must be durable before meta page write

#### Key Insight for Our Design

LMDB demonstrates that **two alternating root locations** (ping-pong) is sufficient for crash consistency. No need for a 128-slot ring (ZFS) or 256-slot ring (RedoxFS) if your only goal is crash recovery. The ring provides history depth for rollback; the ping-pong provides crash safety. These are independent concerns that happen to be served by the same mechanism (a ring is a generalized ping-pong).

---

## Block Device Guarantees

### What Must Be True for COW Crash Consistency

The COW commit protocol requires exactly two guarantees from the block device:

1. **Ordering**: Data blocks written before the root pointer must be durable before the root pointer reaches the media. Violated ordering means the root pointer could reference blocks that were never written (pointing to garbage or old data).

2. **Atomic sector write**: The root pointer write must be all-or-nothing at some granularity. If the root pointer is smaller than the atomic write unit, a torn write is impossible. If larger, a torn root pointer must be detectable (via checksum) and recoverable (via redundant copies).

### virtio-blk

**Specification:** OASIS VIRTIO v1.2, section 5.2

Feature flags relevant to crash consistency:

- `VIRTIO_BLK_F_FLUSH` (bit 9): Device supports cache flush commands (`VIRTIO_BLK_T_FLUSH`, type=4)
- `VIRTIO_BLK_F_CONFIG_WCE` (bit 11): Writeback cache mode available and configurable
- Legacy `VIRTIO_BLK_F_BARRIER` (bit 0): **Deprecated**, not useful in practice
- **No FUA flag** in the standard virtio-blk spec (unlike NVMe). Write durability is achieved via FLUSH commands, not per-request FUA.

**Flush semantics:** A `VIRTIO_BLK_T_FLUSH` request, when completed, guarantees that all previously completed write requests have reached persistent storage. This is the equivalent of `fsync()`.

**What the host does with FLUSH depends on QEMU cache mode:**

- `cache=writethrough`: All writes go to host storage via `fdatasync` before completion. FLUSH is a no-op (already durable). Guest sees no write cache.
- `cache=writeback` (QEMU default when unspecified): Writes complete when in host page cache. FLUSH triggers host `fdatasync`. Guest must issue FLUSH for durability.
- `cache=none`: Writes bypass host page cache (O_DIRECT). Guest must still issue FLUSH.
- `cache=unsafe`: FLUSH commands are silently **ignored**. Data loss on host crash.

**Our QEMU setup** (`run.sh`): `-drive file=$DISK_IMG,if=none,format=raw,id=hd0` — no explicit cache mode, so QEMU defaults to `cache=writeback`. The guest virtio-blk driver MUST issue FLUSH after writing data blocks and before writing the superblock, or crash consistency is not guaranteed.

**Our hypervisor**: Does not implement virtio-blk. Uses 9P for filesystem access. When we implement a filesystem, we will need to either:

- Add a virtio-blk backend to the hypervisor (file-backed, using `fcntl(F_FULLFSYNC)` for flush), or
- Use the 9P path with explicit sync semantics, or
- Run the filesystem on the QEMU path initially

### NVMe

**Specification:** NVM Express Base Specification

Atomicity guarantees:

- **Minimum**: Single logical block (512 bytes or 4 KiB) writes are atomic, even during power failure
- **AWUN** (Atomic Write Unit Normal): Maximum size of an atomic write during normal operation. Often set to 0 (= 1 logical block) on commodity drives
- **AWUPF** (Atomic Write Unit Power Fail): Maximum size of an atomic write guaranteed across power failure. Typically equal to or smaller than AWUN. Many commodity NVMe drives report AWUPF=0 (only single-sector atomicity guaranteed on power loss)
- **NABSN/NABSPF** (Namespace Atomic Boundary): Defines alignment boundaries. A write that crosses a boundary is NOT atomic even if it fits within AWUN

For 16 KiB blocks:

- A 16 KiB write is **NOT guaranteed atomic** on most commodity NVMe unless AWUPF >= 32 sectors (at 512-byte sectors) or >= 4 sectors (at 4K sectors)
- High-capacity SSDs (Samsung, etc.) with 16 KiB internal units may set NABSPF=16 KiB, providing hardware atomicity
- Linux 6.11+ exposes this via `RWF_ATOMIC` flag for `pwritev2()`, with filesystem support in 6.13 (XFS, ext4)

**Practical implication:** Assume 16 KiB writes CAN be torn during power failure unless the specific hardware guarantees otherwise. Design the commit protocol to tolerate torn root pointer writes.

### File-Backed Block Devices (Our Hypervisor)

The hypervisor uses macOS's Hypervisor.framework with guest memory mapped via `mmap`. A file-backed block device would use a regular macOS file.

**macOS fsync vs. F_FULLFSYNC:**

- `fsync()` on macOS flushes from the process to the host kernel, but the disk may still buffer in its own volatile cache
- `fcntl(fd, F_FULLFSYNC)` additionally flushes the disk's volatile write cache. Apple documentation states: "This is not a theoretical edge case. This scenario is easily reproduced with real world workloads and drive power failures."
- APFS (macOS default filesystem) uses COW internally, so file overwrites are internally atomic at the APFS level. But partial writes to a file-backed block device image can still produce torn sectors from the guest's perspective.

**Write atomicity for file-backed devices:**

- A 16 KiB write to a file via `pwrite()` is NOT atomic from the guest's perspective
- APFS may split the write into multiple internal operations
- Power loss during a 16 KiB write could result in any subset of the 16 KiB being written (torn write)
- The only guarantee is that individual APFS blocks (4 KiB on macOS) are COW'd atomically by APFS itself, but the mapping between guest sectors and APFS blocks is not guaranteed to align

**Conclusion:** For our custom hypervisor's virtio-blk backend, we must:

1. Use `fcntl(F_FULLFSYNC)` to implement FLUSH (not just `fsync`)
2. Assume 16 KiB writes can be torn
3. Design the superblock commit protocol to detect and recover from torn writes

---

## Torn Write Protection Strategies

### Strategy 1: Checksum + Redundant Copies (ZFS approach)

Write the root pointer to N locations (ZFS: 128 per label x 4 labels = 512). Each copy includes a checksum (CRC or SHA256). At mount, scan all copies — highest generation with valid checksum wins.

**Pros:** Tolerates torn writes, media errors, and even some firmware bugs. Extremely robust.
**Cons:** Writes N copies per commit. ZFS: 4 writes per device per commit (one per label). For our single-device design, N can be much smaller.

### Strategy 2: Ping-Pong with Checksums (LMDB approach)

Two root pointer locations, alternated each commit. Each includes a generation counter and checksum. At mount, read both, pick highest valid.

**Pros:** Minimal overhead (2 locations). Simple implementation.
**Cons:** Only 1 fallback depth. If both locations are torn (extremely unlikely — would require two consecutive crashes during commits), data loss occurs. No historical rollback from the ring itself.

### Strategy 3: Ring Buffer with Checksums (RedoxFS approach)

N root pointer slots in a ring (RedoxFS: 256). Write to `ring[gen % N]`. Scan on mount.

**Pros:** Provides both crash safety AND rollback depth. Ring naturally supports "undo to previous state."
**Cons:** More metadata space (N \* root_pointer_size). Mount scan is O(N).

### Strategy 4: Double-Write Buffer (MySQL InnoDB)

Write page to a dedicated staging area first, fsync, then write to final location, fsync. Recovery: if final location is torn, copy from staging area.

**Pros:** Works without COW. Guarantees atomicity for in-place-update systems.
**Cons:** 2x write amplification. Unnecessary for COW filesystems (the old blocks ARE the staging area).

### Strategy 5: Sector-Aligned Root Pointer

Make the root pointer fit within a single atomic-write unit. If the hardware guarantees 4 KiB atomic writes, keep the superblock within 4 KiB. If only 512 bytes are guaranteed atomic, keep the critical fields (generation + root block pointer) within 512 bytes.

**Pros:** Zero overhead — relies purely on hardware atomicity.
**Cons:** Hardware-dependent. Doesn't protect against firmware bugs. No detection of corruption (needs checksum anyway). Not sufficient alone.

### Strategy 6: No Barriers Required (Design Around Reordering)

If the block device may reorder writes (no reliable flush), an alternative is to make the commit protocol tolerate arbitrary reordering:

- **Never reuse blocks until N+2 generations after free.** Ensures that even if the root pointer is written before data, the data from the previous generation is still on disk. (ZFS's TXG_DEFER_SIZE=3 is exactly this pattern for ZIL blocks.)
- **All block pointers include checksums.** If the root pointer references a block whose checksum doesn't match, the block wasn't actually written — treat the root pointer as invalid and fall back.
- **Merkle-tree validation from root to leaf.** Every block pointer validates its child. A partially-committed tree is detected at any depth and the last fully-valid tree is used.

**Pros:** Works on the simplest possible block device (no barriers needed). Self-healing at every level.
**Cons:** Deferred block reuse costs space. Full Merkle verification on mount is expensive for large filesystems (but our filesystem is small — single disk, document-oriented).

---

## Implications for Our Filesystem (Model D)

### Our Commit Protocol (Proposed)

Given our constraints (single-writer, pure COW, 16 KiB blocks, virtio-blk backed by host file, no need for multi-device redundancy), the commit protocol should be:

**Superblock ring:** 16 slots of 16 KiB each = 256 KiB at the start of the disk. Each slot contains:

- Magic number (8 bytes)
- Generation counter (u64)
- Checksum (CRC32 covering everything after)
- Root inode block pointer
- Block allocator state pointer
- Snapshot metadata pointer
- Filesystem statistics (total blocks, used blocks, file count)

**Commit sequence:**

1. Write all new/modified data blocks to free locations
2. Write new inode blocks (with updated extent lists)
3. Write updated block allocator metadata
4. Issue FLUSH (virtio: `VIRTIO_BLK_T_FLUSH`)
5. Write new superblock to `ring[generation % 16]`
6. Issue FLUSH

**Crash scenarios:**

- Crash during steps 1-3: Old superblock still points to old blocks. New blocks are orphaned (detected and reclaimed by allocator GC on next mount).
- Crash during step 4 (flush): Same as above — superblock unchanged.
- Crash during step 5: Superblock is torn. CRC32 check fails. Previous ring slot used.
- Crash during step 6: Superblock is written but flush didn't complete. On remount, this is fine — the superblock IS written, we just didn't confirm it. If it's actually torn, CRC catches it.

**Deferred block reuse:** Freed blocks must not be reused for at least 2 generations (similar to ZFS's TXG_DEFER_SIZE). This ensures that even without reliable flush, the blocks referenced by the previous superblock are still on disk.

### Adapting to Our Block Device Stack

**QEMU path (current):**

- virtio-blk with default `cache=writeback`
- Guest driver must issue `VIRTIO_BLK_T_FLUSH` before and after superblock write
- QEMU translates FLUSH to host `fdatasync()` — sufficient on Linux
- On macOS host, QEMU should use `F_FULLFSYNC` but may not — potential gap

**Hypervisor path (future):**

- Need to implement virtio-blk backend in the hypervisor
- FLUSH handler: `fcntl(fd, F_FULLFSYNC)` on the backing file
- This provides the strongest guarantee available on macOS
- Alternative: use writethrough mode (fsync every write) — simpler but slower

**Both paths:**

- 16 KiB writes are NOT atomically guaranteed
- Superblock ring + CRC32 provides detection and recovery
- Deferred block reuse provides defense-in-depth against reordering
- Checksums in block pointers (Merkle-tree style) provide validation at every level

### 16 Slots vs. 128 vs. 256

ZFS uses 128 uberblocks because it serves multi-tenant enterprise storage — rollback depth matters. RedoxFS uses 256 for similar reasons. For our single-user document OS:

- **Crash recovery needs only 2 slots** (ping-pong is sufficient)
- **Rollback depth is handled by the snapshot engine** (not the superblock ring)
- **16 slots provides comfortable margin**: survives 15 consecutive torn writes, which is physically impossible in practice

16 slots x 16 KiB = 256 KiB — same as a single ZFS label. Reasonable overhead.
