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
