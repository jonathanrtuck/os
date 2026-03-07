# Kernel

Bare-metal aarch64 kernel, part of an [OS design exploration](../../design/concept.md). This is a research spike — validating technical foundation decisions by writing real code against the hardware.

Currently boots on QEMU's `virt` machine, drops from EL2 to EL1, sets up the MMU with split TTBR (TTBR1 for kernel, TTBR0 per-process), enables the GIC + generic timer, runs a preemptive scheduler, and spawns a user thread at EL0 with its own address space that prints via syscall. Targets aarch64 only — the assembly, page table setup, and hardware drivers are all ARM-specific. QEMU emulates the hardware, so it runs on any host architecture.

## Prerequisites

- **Rust nightly** with the `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **QEMU** with `qemu-system-aarch64` (e.g. `brew install qemu` on macOS)

## Build & Run

```shell
cd system/kernel
cargo run --release   # builds, then launches QEMU
```

`Ctrl-A X` to exit QEMU.

## What to expect

```shell
booting...
booted.
hello from EL0
```

…not much yet 😬

## Source layout

```txt
src/
  boot.S        — boot trampoline, coarse page tables, EL2→EL1 drop
  exception.S   — exception vectors, context save/restore (upper VA)
  main.rs       — kernel entry, IRQ/SVC dispatch, user thread spawn
  memory.rs     — TTBR1 L3 refinement, W^X, PA/VA conversion
  heap.rs       — bump allocator (16 MiB)
  page_alloc.rs — free-list 4 KiB frame allocator
  asid.rs       — 8-bit ASID allocator
  addr_space.rs — per-process TTBR0 page tables (4-level)
  scheduler.rs  — round-robin preemptive scheduler, TTBR0 swap
  thread.rs     — kernel + user thread creation
  syscall.rs    — syscall dispatcher (exit, write, yield)
  user_test.rs  — EL0 test stub (hello world via syscalls)
  gic.rs        — GICv2 distributor + CPU interface
  timer.rs      — ARM generic timer (EL1 physical, 10 Hz)
  uart.rs       — PL011 UART driver (TX only)
  mmio.rs       — volatile MMIO helpers
link.ld         — linker script
```

## References

- [bahree/rust-microkernel](https://github.com/bahree/rust-microkernel) — primary reference for the initial boot sequence and assembly. The boot.S structure, exception vectors, and context save/restore originated there, with modifications for W^X page table permissions, GICv2 interrupt handling, and the scheduler's context switch model.
