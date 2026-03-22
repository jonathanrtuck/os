# system

Bare-metal aarch64 system for QEMU's `virt` machine. Microkernel architecture: the kernel provides memory, scheduling, and IPC; everything else runs in userspace. Part of a [document-centric OS](../design/foundations.md).

Boots with 4 SMP cores, spawns a single init process (proto-OS-service) which reads a device manifest, spawns virtio drivers and a compositor, and orchestrates a full display pipeline — compositor draws a demo scene with TrueType text, GPU driver presents it to the screen.

## Prerequisites

- **Rust nightly** with the `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- **QEMU** with `qemu-system-aarch64` (e.g. `brew install qemu` on macOS)

## Build & Run

```shell
cd system
cargo run -r   # builds everything, then launches QEMU
```

`Ctrl-A X` to exit QEMU.

A single `cargo build -r` compiles the full system: shared libraries (sys, virtio, drawing), all userspace programs, init (which embeds them), and the kernel (which embeds init). See `build.rs` for the build order.

## Testing

```shell
# Host-side unit tests (1,462 tests across 54 files):
cd system/test && cargo test -- --test-threads=1

# QEMU smoke test (builds, boots, checks expected output):
cd system/test && ./smoke.sh
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

```text
system/
  Cargo.toml                 — build root (builds the entire system)
  build.rs                   — compiles libraries → programs → init → kernel
  run.sh                     — VM launcher (hypervisor default, QEMU=1 for QEMU)
  test/smoke.sh              — builds, boots QEMU, checks expected output
  rust-toolchain.toml        — pins Rust nightly + aarch64-unknown-none target
  DESIGN.md                  — userspace architecture record

  kernel/                    — bare-metal aarch64 microkernel (33 .rs files + 2 .S + link.ld)
    main.rs                  — kernel entry, IRQ/SVC dispatch, boot logging
    boot.S                   — boot trampoline, page tables, EL2→EL1, secondary entry
    exception.S              — exception vectors, context save/restore
    link.ld                  — kernel linker script (upper VA, split TTBR)
    DESIGN.md                — kernel architecture decisions
    README.md                — kernel features, scope, limitations
    ...                      — scheduler, memory, processes, IPC, devices (see kernel/README.md)

  services/                  — trusted userspace (EL0, blue layer)
    init/main.rs             — root task (embeds ELFs, spawns drivers, wires IPC)
    core/                    — OS service (sole writer, scene graph builder, input router)
    compositor/              — scene graph renderer, surface compositing, damage tracking
    drivers/
      virtio-blk/main.rs     — virtio block driver (interrupt-driven, reads sectors)
      virtio-console/main.rs — virtio console driver (TX, interrupt-driven)
      virtio-gpu/main.rs     — virtio-gpu 2D driver (6 commands, presents framebuffer)
      virtio-input/main.rs   — keyboard + tablet input (evdev translation, IPC forwarding)
      virtio-9p/main.rs      — host filesystem passthrough (9P2000.L protocol)

  libraries/                 — shared libraries, used by services and user programs (compiled as rlibs)
    sys/lib.rs               — syscall wrappers + GlobalAlloc + panic handler
    virtio/lib.rs            — virtio MMIO transport + split virtqueue
    drawing/                 — surfaces, colors, PNG decoder, compositing, palette
    fonts/                   — TrueType rasterizer, subpixel rendering, glyph cache
    scene/lib.rs             — scene graph nodes, shared memory layout, text layout
    ipc/lib.rs               — lock-free SPSC ring buffers on shared memory
    protocol/lib.rs          — IPC message types + payload structs (all 9 protocol modules)
    link.ld                  — shared userspace linker script (base VA 0x400000)

  user/                      — untrusted userspace
    text-editor/main.rs      — editor process (input → write requests via IPC)
    echo/main.rs             — echo process (IPC demo)
    stress/main.rs           — IPC stress test program
    fuzz/main.rs             — fuzzing harness
    fuzz-helper/main.rs      — fuzzing helper

  test/                      — host-side unit tests (1,462 tests across 54 files)
    tests/                   — test files (include kernel/library source via #[path])
```
