# prototype

Host-side prototypes for validating OS design ideas before building the real no_std implementations. These run on macOS with full std support.

## Subdirectories

- `files/` -- File abstraction prototype: the `Files` trait (12 operations) with a macOS-backed implementation (`HostFiles`)
- `fs/` -- COW filesystem prototype: block device trait, superblock ring, free-extent allocator, inodes, snapshots, crash consistency

Both prototypes informed the final no_std implementations in `system/libraries/fs/` and `system/libraries/store/`.
