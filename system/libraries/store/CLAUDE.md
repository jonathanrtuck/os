# store

Document store metadata layer over `fs::Files`. Adds media types, queryable attributes, and a persistent catalog to the raw filesystem. The catalog is itself a file within the filesystem, referenced via the root pointer. `no_std` with `alloc`.

## Key Files

- `lib.rs` -- `Store` struct (wraps `Box<dyn Files>`), `CatalogEntry` (media_type + attributes), `DocumentMetadata`, `Query` enum (MediaType, Type prefix, Attribute, And, Or), `StoreError`
- `serialize.rs` -- Binary catalog serialization/deserialization: magic + entry_count + per-entry (file_id, media_type, attributes)

## Dependencies

- `fs` -- `Files` trait, `FileId`, `SnapshotId`, `FsError`

## Conventions

- Catalog format uses a `u32` magic number ("CATL") for validation
- Queries support equality, prefix matching on type, attribute key-value, and boolean combinators (AND/OR)
- The store delegates all file I/O and snapshots to the underlying `Files` implementation
- Media type is fundamental OS-managed metadata, not a userspace convention
