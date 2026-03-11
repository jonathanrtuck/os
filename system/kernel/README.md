# Kernel

Bare-metal aarch64 kernel targeting QEMU's `virt` machine. Part of a [document-centric OS](../../design/concept.md).

Boots with 4 SMP cores via PSCI, drops from EL2 to EL1, sets up the MMU with split TTBR (TTBR1 for kernel, TTBR0 per-process), and runs a preemptive EEVDF scheduler with handle-based scheduling contexts. Spawns a single init process (proto-OS-service) which reads a device manifest, spawns virtio drivers and a compositor, allocates a shared framebuffer, and orchestrates a full display pipeline — compositor draws a demo scene, GPU driver presents it to the screen. Targets aarch64 only — the assembly, page table setup, and hardware drivers are all ARM-specific. QEMU emulates the hardware, so it runs on any host architecture.

## Features

- **SMP** — 4 cores via PSCI CPU_ON, per-core stacks/timers/GIC
  - Ticket spinlock with IRQ masking for all shared state
- **Preemptive scheduling** — EEVDF (Earliest Eligible Virtual Deadline First), 250 Hz timer tick
  - Proportional-fair CPU sharing with latency differentiation (shorter slice = earlier deadline)
  - Scheduling contexts: handle-based kernel objects (budget/period) for temporal isolation
  - Context donation: OS service borrows editor's context to bill rendering work correctly
  - Kernel enforces mechanism + algorithm; OS service owns policy (content-type-aware budgets)
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
  - Virtio-mmio v2: block (read sectors), console (TX), GPU (2D display)
- **Display pipeline** — init → DMA framebuffer → shared memory → compositor → GPU driver → pixels on screen
  - Compositor draws demo scene using drawing library (fill, rect, text, blit)
  - GPU driver presents via virtio-gpu 2D protocol (6 commands)

## Prerequisites

- **Rust nightly** with the `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **QEMU** with `qemu-system-aarch64` (e.g. `brew install qemu` on macOS)

## Build & Run

```shell
cd system
cargo run --release   # builds, then launches QEMU
```

`Ctrl-A X` to exit QEMU.

## Testing

```shell
# Host-side unit tests (257 tests: EEVDF, scheduling contexts, handles, ELF, DTB, VMA, buddy, slab, heap, heap routing, ASID, virtqueue, drawing, waitable):
cd system/test && cargo test -- --test-threads=1

# QEMU smoke test (builds, boots, checks output):
cd system && ./smoke-test.sh
```

## What to expect

```console
🥾 booting…
  💾 memory - 256mib ram, w^x page tables
  📦 heap - 16mib (linked-list + slab)
  🌳 dtb - 40 devices discovered
  🧩 frames - 60309 free (buddy allocator, 4k–4m)
  ⚡ interrupts - gic v2 (dtb)
  📋 scheduler - eevdf + scheduling contexts
  🔌 virtio - 2 devices found
  🔀 processes - init started with device manifest
  🧵 smp - booting secondaries via psci
  ✓ core 1 online
  ✓ core 2 online
  ✓ core 3 online
  ⏱️  timer - 250hz
🥾 booted.
  🔧 init - proto-os-service starting
     2 devices in manifest
     device 0: id=16
     spawning driver (elf 744816 bytes)
       ...
     device 1: id=2
     spawning driver (elf 743272 bytes)
       ...
     spawned driver: blk
     setting up display pipeline
  🔌 virtio - blk capacity=2048 sectors
     sector 0 - HELLO VIRTIO BLK
     framebuffer: 1024x768 (4096 KiB)
       ...
     compositor started, waiting
  🎨 compositor - starting
     scene drawn, signaling init
     compositor done, starting gpu driver
  🖥️  virtio-gpu ready
     display 1280x800
     presented to display
     display pipeline complete
  🔧 init - done
```

Boot initializes each subsystem in dependency order and logs progress. The emoji prefix identifies the subsystem. Secondary cores report in asynchronously (order may vary). The kernel spawns only init, which reads the device manifest and orchestrates everything: spawns virtio-blk (reads a sector), allocates a shared framebuffer, spawns a compositor (draws a demo scene), then starts the GPU driver to present it to the QEMU display.

## Source layout

```txt
src/
  boot.S                   — boot trampoline, coarse page tables, EL2→EL1 drop, secondary entry
  exception.S              — exception vectors, context save/restore (upper VA)
  main.rs                  — kernel entry, IRQ/SVC dispatch, boot logging, memory map
  context.rs               — CPU register state struct (matches exception.S offsets)
  process.rs               — process creation from ELF binaries (demand-paged VMAs)
  executable.rs            — pure functional ELF64 parser (PT_LOAD segments)
  device_tree.rs           — FDT parser (discovers hardware from firmware device tree)
  futex.rs                 — fast userspace mutex (PA-keyed wait table, lost-wakeup prevention)
  memory.rs                — TTBR1 L3 refinement, W^X, PA/VA conversion
  heap.rs                  — linked-list allocator (first-fit, coalescing, 16 MiB) + slab routing
  slab.rs                  — power-of-two slab caches (64–2048B) for small kernel objects
  page_allocator.rs        — buddy allocator for contiguous 2^n page frames (4 KiB–4 MiB)
  address_space_id.rs      — 8-bit ASID allocator (generation-based recycling, lazy revalidation)
  address_space.rs         — per-process TTBR0 page tables (4-level), demand paging fault handler
  memory_region.rs         — virtual memory area tracking (sorted list, binary search lookup)
  channel.rs               — IPC channels (shared memory ring buffers + signal/wait notification)
  handle.rs                — per-process handle table (256 slots, read/write rights)
  paging.rs                — page table constants, memory layout, user VA map
  sync.rs                  — IrqMutex (ticket spinlock + IRQ masking, SMP-safe)
  scheduling_algorithm.rs  — pure EEVDF algorithm (vruntime, eligibility, virtual deadline)
  scheduling_context.rs    — pure budget/period logic (charge, replenish)
  scheduler.rs             — SMP-aware EEVDF scheduler, scheduling context management, per-core state
  thread.rs                — thread struct, state machine (Ready/Running/Blocked/Exited), scheduling fields
  metrics.rs               — atomic counters (syscalls, page faults, context switches, lock contention)
  process_exit.rs          — process exit notification (waitable handles)
  thread_exit.rs           — thread exit notification (waitable handles)
  waitable.rs              — generic WaitableRegistry<Id> (shared pattern for exit/timer/interrupt)
  interrupt.rs             — interrupt registration table (IRQ forwarding to userspace handles)
  syscall.rs               — syscall dispatcher (25 syscalls)
  per_core.rs              — per-core data structures (online flag, core ID via MPIDR)
  power.rs                 — PSCI CPU_ON wrapper (HVC #0) for secondary core boot
  interrupt_controller.rs  — GICv2 distributor + CPU interface (per-core init)
  timer.rs                 — ARM generic timer (EL1 physical, 250 Hz, SMP per-core PPI)
  serial.rs                — PL011 UART driver (TX only, SMP-safe locking)
  memory_mapped_io.rs      — volatile MMIO helpers (read8/read32/write8/write32)

(build infrastructure — Cargo.toml, build.rs, smoke-test.sh — is at the system/ level, not in kernel/)

../services/
  init/main.rs             — proto-OS-service (embeds all ELFs, spawns drivers + compositor, display pipeline)
  compositor/main.rs       — toy compositor (draws demo scene into shared framebuffer)
  drivers/
    virtio-blk/main.rs     — userspace virtio block driver (interrupt-driven, reads sectors)
    virtio-console/main.rs — userspace virtio console driver (TX, interrupt-driven)
    virtio-gpu/main.rs     — userspace virtio-gpu 2D driver (6 core commands, presents framebuffer)

../libraries/
  sys/lib.rs               — userspace syscall wrappers + panic handler (compiled as rlib)
  virtio/lib.rs            — virtio MMIO transport + split virtqueue (compiled as rlib)
  drawing/lib.rs           — drawing primitives + 8×16 bitmap font (compiled as rlib)
  link.ld                  — shared userspace linker script (base VA 0x400000)

../user/
  echo/main.rs             — echo process (IPC pong responder, demo)

../test/
  tests/eevdf.rs           — EEVDF algorithm tests (eligibility, selection, vruntime)
  tests/sched_context.rs   — scheduling context tests (budget, replenishment, charge)
  tests/handle.rs          — handle table unit tests (insert, close, rights, full table)
  tests/executable.rs      — ELF parser unit tests (valid/invalid binaries)
  tests/device_tree.rs     — FDT parser unit tests (FdtBuilder constructs minimal blobs)
  tests/vma.rs             — VMA lookup/insert unit tests (includes memory_region.rs)
  tests/buddy.rs           — buddy allocator tests (mock IrqMutex)
  tests/slab.rs            — slab size-class selection tests
  tests/heap.rs            — heap allocator tests (alloc, free, coalescing)
  tests/heap_routing.rs    — heap↔slab dealloc routing (cross-allocator contamination prevention)
  tests/asid.rs            — ASID allocator tests (generation rollover)
  tests/virtqueue.rs       — virtqueue descriptor chain validation tests
  tests/channel.rs         — channel creation and shared page allocation tests
  tests/futex.rs           — futex wait/wake tests (PA-keyed)
  tests/drawing.rs         — drawing primitives + font rendering tests (41 tests)
  tests/waitable.rs        — WaitableRegistry tests (readiness, notify, destroy)

```

## Scope & Limitations

**Hardware target:**

- QEMU `virt` machine (aarch64) with GICv2
- DTB parser discovers hardware; GIC + virtio-mmio addresses wired to device init
- Virtio-mmio v2 transport (requires QEMU `-global virtio-mmio.force-legacy=false`)
- 4 cores tested, up to 8 supported (via `MAX_CORES` constant)

**Current limitations:**

- Virtio block: read-only (no writes)
- Virtio console: TX only (no RX)
- Global scheduler lock (fine for ≤8 cores)
- 256-slot handle table per process (fixed, no growth)
- Linked-list heap for allocations >2 KiB (slab for ≤2 KiB)
- User fault handler doesn't reliably print diagnostics before killing processes

**Not targeted:** x86_64, POSIX, network stack, hard realtime.

**Blocked on OS design decisions:** filesystem (COW required by undo architecture), full syscall surface, OS service process.

## Architecture

Design rationale for every kernel subsystem — alternatives considered, tradeoffs, and why each approach was chosen — is documented in [`DESIGN.md`](DESIGN.md).

## References

- [bahree/rust-microkernel](https://github.com/bahree/rust-microkernel) — primary reference for the initial boot sequence and assembly. The boot.S structure, exception vectors, and context save/restore originated there, with modifications for W^X page table permissions, GICv2 interrupt handling, and the scheduler's context switch model.
