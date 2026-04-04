# fs

COW filesystem library. Implements a copy-on-write filesystem with the `BlockDevice` trait abstracting storage. The `Files` trait is the primary interface consumed by the document service. `no_std` with `alloc`.

## Key Files

- `lib.rs` -- `Files` trait (create, read, write, truncate, snapshot, restore, commit), `FileId`, `SnapshotId`, `FsError`, `BLOCK_SIZE` (16 KiB)
- `filesystem.rs` -- `Filesystem<D: BlockDevice>` implementing `Files` over any block device
- `block.rs` -- `BlockDevice` trait (read_block, write_block, block_count)
- `superblock.rs` -- Superblock ring (8-entry circular buffer with CRC32 validation, two-flush commit)
- `alloc_mod.rs` -- Free-extent allocator for block allocation
- `inode.rs` -- Inode with inline data (small files), extent lists (16 inline + 1364 overflow via indirect block)
- `snapshot.rs` -- COW snapshot creation and restore (atomic multi-file snapshots)
- `crc32.rs` -- CRC32 checksums for on-disk integrity

## Dependencies

- None

## Conventions

- Block size is 16 KiB (matches kernel page size)
- `commit()` is the transaction boundary; uncommitted writes are lost on crash
- Superblock uses an 8-entry ring with CRC32 for crash recovery
- Snapshots are atomic across multiple files (used for undo/redo)
- The filesystem is semantically ignorant -- media types and metadata live in the `store` layer above
