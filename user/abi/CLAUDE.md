# abi

Typed Rust wrappers over the kernel's SVC interface. `#![no_std]`, zero
dependencies, bare-metal only (`aarch64-unknown-none`).

## ABI is frozen

The kernel ABI (syscall numbers, register conventions, error codes) is frozen.
Changes here cascade to every userspace crate. Before modifying any syscall
number, argument layout, or error variant:

1. Confirm the kernel side has already landed the change
2. Update `raw::num` constants to match `kernel/src/syscall.rs`
3. Grep all callers — every service and library that imports `abi`

Do not add, remove, or renumber syscalls without explicit authorization.

## Structure

- `raw.rs` -- SVC #0 invocation, syscall number constants (0-34)
- `types.rs` -- `Handle`, `ThreadId`, `SyscallError`, shared ABI types
- `vmo.rs`, `ipc.rs`, `event.rs`, `thread.rs`, `space.rs`, `handle.rs`,
  `system.rs` -- typed wrappers per kernel object

## Build

Bare-metal only. `.cargo/config.toml` sets `target = aarch64-unknown-none`. Not
in the workspace (excluded in root `Cargo.toml`).
