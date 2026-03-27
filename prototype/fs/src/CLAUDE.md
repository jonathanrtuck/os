# prototype/fs/src

Source files for the COW filesystem prototype.

- `lib.rs` -- Public API, re-exports, `Files` trait implementation delegation
- `block.rs` -- `BlockDevice` trait and implementations (file-backed, in-memory, logging wrapper)
- `superblock.rs` -- Superblock ring buffer for atomic metadata commits
- `alloc.rs` -- Free-extent allocator for block management
- `inode.rs` -- Inode structure with inline data and extent-based storage
- `filesystem.rs` -- `Filesystem<D>`: ties together superblock, allocator, inodes; two-flush commit protocol
- `snapshot.rs` -- Per-file COW snapshots (O(file_size) copy, not per-block COW)
- `crc32.rs` -- CRC32 checksums for data integrity
