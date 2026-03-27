# sys

Syscall wrappers and userspace heap allocator. Provides safe Rust functions for each of the 28 kernel syscalls and implements `GlobalAlloc` backed by the `memory_alloc` syscall. Every userspace binary links against this. `no_std`, no dependencies.

## Key Files

- `lib.rs` -- Crate root: module declarations, `pub use` re-exports, `#[global_allocator]`, `#[panic_handler]`, PAGE_SIZE constant
- `types.rs` -- Typed handle wrappers (`ChannelHandle`, `InterruptHandle`, `ProcessHandle`, `TimerHandle`, `ThreadHandle`, `SchedHandle`), `HeapStats`, `SyscallResult`
- `error.rs` -- `SyscallError` enum and `from_raw()` decoder
- `asm.rs` -- Syscall number constants (`mod nr`), inline assembly glue (`syscall0` through `syscall4`), `result()` helper, `align_up()`
- `heap.rs` -- `UserHeap` global allocator: free-list with coalescing, spinlock-protected, grows via `memory_alloc` pages. `HeapStats` instrumentation
- `counter.rs` -- `counter()` and `counter_freq()` for sub-ms timing via CNTVCT_EL0/CNTFRQ_EL0, `counter_to_ns()` conversion
- `syscalls.rs` -- 27 public syscall wrappers (exit, write, yield, channel_create, channel_signal, wait, timer_create, interrupt_register, device_map, dma_alloc, process_create, process_start, memory_share, memory_alloc, memory_free, thread_create, futex_wait, futex_wake, process_set_syscall_filter, etc.)
- `io.rs` -- `print()`, `write()`, `format_u32()`, `print_u32()` output utilities

## Dependencies

- None

## Conventions

- ABI: syscall number in x8, arguments in x0..x5, return in x0. Invoke via `svc #0`
- Negative return values are errors, decoded into `SyscallError`
- Heap allocator: free-list with coalescing, spinlock-protected, grows via `memory_alloc` pages
- Handle types are `#[repr(transparent)]` over `u8` -- zero-cost compile-time safety
- `counter()` / `counter_freq()` read aarch64 virtual timer registers (enabled by kernel)
