# prototype/files/src

Source files for the file abstraction prototype.

- `lib.rs` -- `Files` trait definition, `FileId`/`SnapshotId`/`SnapshotInfo` types
- `host.rs` -- `HostFiles`: macOS filesystem-backed implementation (files in `{base}/files/`, snapshots in `{base}/snapshots/`)
- `tests.rs` -- Unit tests for the `Files` trait contract
