# prototype/fs

Host-side prototype of the COW filesystem. Implements the design from `design/journal.md` as a regular Rust binary on macOS.

The `BlockDevice` trait abstracts storage, allowing the same filesystem code to run against a file-backed device (host prototype), an in-memory device (unit tests), or a logging wrapper (crash consistency tests). Uses proptest for property-based testing.

Architecture: `Core -> Files trait -> Filesystem<D> -> BlockDevice -> disk`

This prototype was the basis for the no_std implementation in `system/libraries/fs/`.
