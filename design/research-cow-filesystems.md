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
