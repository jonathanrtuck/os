# prototype/files

Host-side prototype of the `Files` trait -- the filesystem interface the OS service calls for document operations.

Defines `FileId`, `SnapshotId`, and 12 file operations (create, read, write, truncate, snapshot, restore, delete, list). `HostFiles` implements the trait using regular macOS files and directories. Snapshots are full file copies (the real implementation uses COW block sharing).

This prototype validated the interface before the no_std implementation in `system/libraries/fs/`.
