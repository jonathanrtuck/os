# Kernel

Bare-metal aarch64 kernel targeting QEMU's `virt` machine. Part of a [document-centric OS](../../design/concept.md).

Boots with 4 SMP cores via PSCI, drops from EL2 to EL1, sets up the MMU with split TTBR (TTBR1 for kernel, TTBR0 per-process), and runs a preemptive priority scheduler. Two user processes at EL0 exchange messages over shared-memory IPC, then exit — the kernel fully reclaims all resources. Targets aarch64 only — the assembly, page table setup, and hardware drivers are all ARM-specific. QEMU emulates the hardware, so it runs on any host architecture.

## Features

- **SMP** — 4 cores via PSCI CPU_ON, per-core stacks/timers/GIC
  - Ticket spinlock with IRQ masking for all shared state
- **Preemptive scheduling** — priority-based (idle/normal/high), 250 Hz timer tick
  - Global run queue, O(1) priority selection, round-robin within levels
- **Virtual memory** — 4-level page tables, per-process address spaces
  - Split TTBR: TTBR1 for kernel (shared), TTBR0 per-process (swapped on context switch)
  - W^X enforcement — no page is both writable and executable
  - Demand paging via VMAs (pages allocated on fault, not at spawn)
  - 8-bit ASID with generation-based recycling
- **Memory management** — layered allocator strategy
  - Buddy allocator for contiguous page frames (4 KiB – 4 MiB)
  - Slab caches for small kernel objects (64 – 2048 bytes, O(1))
  - Linked-list heap with coalescing for variable-size allocations
- **Processes** — ELF64 loading, per-process address spaces, full cleanup on exit
  - User code at EL0, kernel at EL1
  - 16 KiB user stack with guard page
  - Per-process handle table (256 slots, read/write rights)
- **IPC** — shared-memory channels with signal/wait notification
  - Handle-based access control, kernel-mediated creation
  - Lost-wakeup safe (persistent signal flag)
- **Devices**
  - GICv2 interrupt controller (distributor + per-core CPU interface)
  - ARM generic timer (EL1 physical, per-core PPI)
  - PL011 UART (TX, SMP-safe)
  - Virtio-mmio v2: block device (read sectors) + console (TX)

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
# Host-side unit tests (handle table, ELF parser, VMA, buddy allocator, slab):
cd system/host-tests && cargo test -- --test-threads=1

# QEMU smoke test (builds, boots, checks output):
cd system/kernel && ./smoke-test.sh
```

## What to expect

```console
🥾 booting…
  💾 memory - 256mib ram, w^x page tables
  📦 heap - 16mib (linked-list + slab)
  🧩 frames - 60888 free (buddy allocator, 4k–4m)
  ⚡ interrupts - gic v2
  📋 scheduler - priority queues (idle/normal/high)
  🔌 virtio - blk capacity=2048 sectors
     sector 0 - HELLO VIRTIO BLK
  🔀 processes - init + echo, ipc channel
  🧵 smp - booting secondaries via psci
  ✓ core 1 online
  ✓ core 2 online
  ✓ core 3 online
  ⏱️  timer - 250hz
🥾 booted.
echo recv: ping
init recv: pong
```

Boot initializes each subsystem in dependency order and logs progress. The emoji prefix identifies the subsystem. Secondary cores report in asynchronously (order may vary). Two user processes exchange messages over shared-memory IPC, then exit cleanly.

## Source layout

```txt
src/
  boot.S        — boot trampoline, coarse page tables, EL2→EL1 drop, secondary entry
  exception.S   — exception vectors, context save/restore (upper VA)
  main.rs       — kernel entry, IRQ/SVC dispatch, boot logging, memory map
  context.rs    — CPU register state struct (matches exception.S offsets)
  process.rs    — process creation from ELF binaries (demand-paged VMAs)
  elf.rs        — pure functional ELF64 parser (PT_LOAD segments)
  memory.rs     — TTBR1 L3 refinement, W^X, PA/VA conversion
  heap.rs       — linked-list allocator (first-fit, coalescing, 16 MiB) + slab routing
  slab.rs       — power-of-two slab caches (64–2048B) for small kernel objects
  page_alloc.rs — buddy allocator for contiguous 2^n page frames (4 KiB–4 MiB)
  asid.rs       — 8-bit ASID allocator (generation-based recycling, lazy revalidation)
  addr_space.rs — per-process TTBR0 page tables (4-level), demand paging fault handler
  vma.rs        — virtual memory area tracking (sorted list, binary search lookup)
  channel.rs    — IPC channels (shared memory ring buffers + signal/wait notification)
  handle.rs     — per-process handle table (256 slots, read/write rights)
  paging.rs     — page table constants, memory layout, user VA map
  sync.rs       — IrqMutex (ticket spinlock + IRQ masking, SMP-safe)
  scheduler.rs  — SMP-aware priority scheduler (idle/normal/high), per-core state
  thread.rs     — thread struct, state machine (Ready/Running/Blocked/Exited), priorities
  syscall.rs    — syscall dispatcher (exit, write, yield, handle_close, signal, wait)
  percpu.rs     — per-core data structures (online flag, core ID via MPIDR)
  psci.rs       — PSCI CPU_ON wrapper (HVC #0) for secondary core boot
  gic.rs        — GICv2 distributor + CPU interface (per-core init)
  timer.rs      — ARM generic timer (EL1 physical, 250 Hz, SMP per-core PPI)
  uart.rs       — PL011 UART driver (TX only, SMP-safe locking)
  mmio.rs       — volatile MMIO helpers (read8/read32/write8/write32)
  virtio/
    mod.rs      — virtio-mmio v2 transport: probe, feature negotiation, device setup
    virtqueue.rs — split virtqueue (descriptor table + available/used rings, DMA)
    blk.rs      — virtio block driver (read sectors via 3-descriptor chain)
    console.rs  — virtio console driver (TX only, demo)
build.rs        — compiles user processes → ELF at build time

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
  tests/vma.rs    — VMA lookup/insert unit tests (host-side)
  tests/buddy.rs  — buddy allocator tests (host-side, mock IrqMutex)
  tests/slab.rs   — slab size-class selection tests (host-side)

smoke-test.sh     — QEMU boot + output verification (17 checks)
```

## Scope & Limitations

**Hardware target:**

- QEMU `virt` machine (aarch64) with GICv2
- Hardcoded device addresses (no device tree parsing)
- Virtio-mmio v2 transport (requires QEMU `-global virtio-mmio.force-legacy=false`)
- 4 cores tested, up to 8 supported (via `MAX_CORES` constant)

**Current limitations:**

- Virtio block: read-only (no writes), polling (no interrupts)
- Virtio console: TX only (no RX)
- Global scheduler lock (fine for ≤8 cores)
- 256-slot handle table per process (fixed, no growth)
- Linked-list heap for allocations >2 KiB (slab for ≤2 KiB)

**Not targeted:** x86_64, POSIX, network stack, hard realtime, device tree.

**Blocked on OS design decisions:** filesystem (COW required by undo architecture), full syscall surface, OS service process.

## Architecture

Design rationale for every kernel subsystem — alternatives considered, tradeoffs, and why each approach was chosen — is documented in [`DESIGN.md`](DESIGN.md).

## References

- [bahree/rust-microkernel](https://github.com/bahree/rust-microkernel) — primary reference for the initial boot sequence and assembly. The boot.S structure, exception vectors, and context save/restore originated there, with modifications for W^X page table permissions, GICv2 interrupt handling, and the scheduler's context switch model.
