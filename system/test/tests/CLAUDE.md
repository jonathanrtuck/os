# test/tests

72 host-compiled test files exercising kernel and library logic in isolation. Each file includes kernel or library source via `#[path]` with stub dependencies (mock `IrqMutex`, identity PA/VA mapping).

Run with `cargo test -- --test-threads=1` (some tests share global state).

## Naming Conventions

- Files mirror kernel/library module names (e.g., `buddy.rs` tests the page allocator, `eevdf.rs` tests the scheduler)
- `adversarial_*` -- stress/fuzz tests targeting audit findings (boundaries, buddy, churn, pointers, slab, VMA)
- `stress_*` -- concurrent stress tests (allocation fragmentation, buddy coalescing, scheduling contexts)
- `integration_stress.rs` -- cross-subsystem integration under load
- `fs_*` -- filesystem library tests (crash consistency, linked-block operations)
