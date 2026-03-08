# Kernel

Bare-metal aarch64 kernel, part of an [OS design exploration](../../design/concept.md). This is a research spike — validating technical foundation decisions by writing real code against the hardware.

Currently boots on QEMU's `virt` machine, drops from EL2 to EL1, sets up the MMU with split TTBR (TTBR1 for kernel, TTBR0 per-process), enables the GIC + generic timer, and runs a preemptive round-robin scheduler. Spawns two user processes at EL0 (init + echo), each with its own address space, connected via a shared-memory IPC channel. The processes exchange messages (ping/pong), then exit — the kernel fully reclaims all resources (page frames, page tables, ASIDs, handles, kernel stacks, heap allocations). Targets aarch64 only — the assembly, page table setup, and hardware drivers are all ARM-specific. QEMU emulates the hardware, so it runs on any host architecture.

## Prerequisites

- **Rust nightly** with the `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **QEMU** with `qemu-system-aarch64` (e.g. `brew install qemu` on macOS)

## Build & Run

```shell
cd system/kernel
cargo run --release   # builds, then launches QEMU
```

`Ctrl-A X` to exit QEMU.

## Testing

```shell
# Host-side unit tests (handle table, ELF parser):
cd system/host-tests && cargo test

# QEMU smoke test (builds, boots, checks output):
cd system/kernel && ./smoke-test.sh
```

## What to expect

```console
🥾 booting…
🥾 booted.
echo recv: ping
init recv: pong
```

Two user processes exchange messages over shared-memory IPC, then exit cleanly.

## Source layout

```txt
src/
  boot.S        — boot trampoline, coarse page tables, EL2→EL1 drop
  exception.S   — exception vectors, context save/restore (upper VA)
  main.rs       — kernel entry, IRQ/SVC dispatch, memory map
  context.rs    — CPU register state struct (matches exception.S offsets)
  process.rs    — process creation from ELF binaries
  elf.rs        — pure functional ELF64 parser (PT_LOAD segments)
  memory.rs     — TTBR1 L3 refinement, W^X, PA/VA conversion
  heap.rs       — linked-list allocator (first-fit, coalescing, 16 MiB)
  page_alloc.rs — free-list 4 KiB frame allocator
  asid.rs       — 8-bit ASID allocator (with recycling)
  addr_space.rs — per-process TTBR0 page tables (4-level), owned/shared pages
  channel.rs    — IPC channels (shared memory + signal/wait notification)
  handle.rs     — per-process handle table (256 slots, read/write rights)
  paging.rs     — page table constants, memory layout, user VA map
  sync.rs       — IrqMutex (IRQ-masking single-core mutex)
  scheduler.rs  — round-robin preemptive scheduler, TTBR0 swap, thread reaping
  thread.rs     — thread struct, state machine, trust levels, resource cleanup
  syscall.rs    — syscall dispatcher (exit, write, yield, handle_close, signal, wait)
  gic.rs        — GICv2 distributor + CPU interface
  timer.rs      — ARM generic timer (EL1 physical, 10 Hz)
  uart.rs       — PL011 UART driver (TX only)
  mmio.rs       — volatile MMIO helpers
build.rs        — compiles user processes → ELF at build time
link.ld         — kernel linker script

../user/libsys/
  lib.rs        — userspace syscall wrappers + panic handler (compiled as rlib)

../user/init/
  main.rs       — init process (IPC ping initiator)

../user/echo/
  main.rs       — echo process (IPC pong responder)

../user/link.ld — shared userspace linker script (base VA 0x400000)

../host-tests/
  tests/handle.rs — handle table unit tests (host-side)
  tests/elf.rs    — ELF parser unit tests (host-side)

smoke-test.sh     — QEMU boot + output verification
```

## References

- [bahree/rust-microkernel](https://github.com/bahree/rust-microkernel) — primary reference for the initial boot sequence and assembly. The boot.S structure, exception vectors, and context save/restore originated there, with modifications for W^X page table permissions, GICv2 interrupt handling, and the scheduler's context switch model.

---

## Roadmap

The kernel's interface to userspace (syscalls, IPC channels, handle table) is stable. Everything below — allocators, scheduling, synchronization, core management — can be improved without breaking the contract. Phases are ordered by dependency; items within a phase are independent.

| Phase          | Item                 | Current                            | Target                                              |
| -------------- | -------------------- | ---------------------------------- | --------------------------------------------------- |
| **1. SMP**     | 1.1 Sync primitives  | `IrqMutex` (IRQ mask, single-core) | Ticket spinlock + IRQ masking                       |
|                | 1.2 Multi-core boot  | Core 0 only, others parked         | PSCI boot, per-CPU data, per-core stacks/timers     |
|                | 1.3 SMP scheduler    | Round-robin, O(n), no priorities   | Global queue w/ priorities (idle/normal/high), O(1) |
| **2. Memory**  | 2.1 Slab allocator   | Linked-list for all heap allocs    | Slab caches for fixed-size kernel objects           |
|                | 2.2 Buddy allocator  | Free-list, single pages only       | Buddy system, contiguous 2^n page allocation        |
|                | 2.3 ASID recycling   | 255 concurrent max, panics         | Generation-based, lazy re-acquire on context switch |
| **3. Cleanup** | 3.1 Timer resolution | 10 Hz                              | 250 Hz                                              |
|                | 3.2 Boot map cleanup | Identity map wastes memory         | Reclaim boot TTBR0 pages                            |
| **4. VM**      | 4.1 Demand paging    | Eager alloc at process creation    | Fault-driven mapping, VMA tracking                  |
| **5. I/O**     | 5.1 Virtio framework | Hardcoded GIC + UART               | virtio-mmio transport, console + block drivers      |

**Out of scope** (blocked by OS design decisions): filesystem (COW), full syscall surface, OS service process.

**Not planned:** x86_64, POSIX, network stack, hard realtime.

Detailed design notes for each item are in [`docs/design-notes.md`](docs/design-notes.md).
