# system

Bare-metal aarch64 system for QEMU's `virt` machine and the [native macOS hypervisor](https://github.com/jonathanrtuck/hypervisor). Microkernel architecture: the kernel provides memory, scheduling, and IPC; everything else runs in userspace. Part of a [document-centric OS](../design/foundations.md).

Boots with 4 SMP cores, spawns a single init process which reads a device manifest, spawns virtio drivers, loads fonts and images via 9P filesystem, and orchestrates a full display pipeline — core builds a scene graph, a render service presents it to the screen with Metal GPU rendering.

## prerequisites

- **Rust nightly** with the `aarch64-unknown-none` target (handled automatically by `rust-toolchain.toml` — just [install Rust](https://rustup.rs/))
- For QEMU path: **QEMU** with `qemu-system-aarch64` (e.g. `brew install qemu` on macOS)
- For hypervisor path: the [hypervisor](https://github.com/jonathanrtuck/hypervisor) binary (`make install` from that repo)

## build & run

```shell
cd system
cargo run -r   # builds everything, then launches via hypervisor (default)
```

Use `QEMU=1 cargo run -r` for QEMU instead. Close the window or Cmd+Q to exit the hypervisor.

A single `cargo build -r` compiles the full system: shared libraries, all userspace programs, init (which embeds them as ELF blobs), and the kernel (which embeds init). See `build.rs` for the build order.

## testing

```shell
# Host-side unit tests (~2,152 tests):
cd system/test && cargo test -- --test-threads=1

# QEMU smoke test (builds, boots, checks expected output):
cd system/test && ./smoke.sh
```

## what to expect

```console
🥾 booting…
  📦 heap - 16mib (linked-list + slab)
  🌳 dtb - 7 devices discovered
  💾 memory - 256mib ram, w^x page tables
  🧩 frames - 57387 free (buddy allocator, 4k–8m)
  🚨 pvpanic - registered
  ⚡ interrupts - gic v3 (hardcoded)
  📋 scheduler - eevdf + scheduling contexts
  🔌 virtio - 4 devices found
  🕐 rtc - pl031 discovered
  🔀 processes - init started with device manifest
  🧵 smp - booting secondaries via psci
  ✓ core 1 online
  ✓ core 2 online
  ✓ core 3 online
  ⏱️  timer - tickless
🥾 booted.
  🔧 init - proto-os-service starting
     5 devices in manifest
     device 0: id=9      (virtio-9p)
     device 1: id=18     (virtio-input keyboard)
     device 2: id=18     (virtio-input tablet)
     device 3: id=22     (virtio-metal GPU)
     device 4: id=200    (pl031 rtc)
       ...
  📂 virtio-9p - starting
     mono font loaded: 303144 bytes
     sans font loaded: 879708 bytes
     serif font loaded: 1196808 bytes
     png loaded: 884951 bytes
       ...
  🔱 metal-render - starting
     display 4112x2658@120Hz
       ...
  🧠 core - starting
     entering event loop
  📝 text-editor starting
     entering event loop
```

Boot initializes each subsystem in dependency order and logs progress. The emoji prefix identifies the subsystem. Secondary cores report in asynchronously (order may vary). The kernel spawns only init, which reads the device manifest and orchestrates everything: spawns drivers (9P filesystem, keyboard, tablet, GPU), loads fonts and images from the host via 9P, sets up the display pipeline (core → scene graph → render service), spawns the text editor, and enters monitoring mode.

## source layout

```text
system/
  Cargo.toml                 — build root (builds the entire system)
  build.rs                   — compiles libraries → programs → init → kernel
  run.sh                     — VM launcher (hypervisor default, QEMU=1 for QEMU)
  test/                      — host-side unit tests + QEMU integration scripts
  rust-toolchain.toml        — pins Rust nightly + aarch64-unknown-none target
  DESIGN.md                  — userspace architecture record
  share/                     — runtime assets (fonts, images, icons)

  kernel/                    — bare-metal aarch64 microkernel (33 .rs files + 2 .S + link.ld)
    main.rs                  — kernel entry, IRQ/SVC dispatch, boot logging, pvpanic
    boot.S                   — boot trampoline, page tables, EL2→EL1, secondary entry
    exception.S              — exception vectors, context save/restore
    link.ld                  — kernel linker script (upper VA, split TTBR)
    DESIGN.md                — kernel architecture decisions
    README.md                — kernel features, scope, limitations
    ...                      — scheduler, memory, processes, IPC, devices (see kernel/README.md)

  services/                  — trusted userspace (EL0)
    init/main.rs             — root task (embeds ELFs, spawns drivers, wires IPC)
    core/                    — OS service (sole writer, scene graph builder, input router)
    drivers/
      metal-render/          — Metal render service (native Metal via hypervisor)
      cpu-render/            — CPU render service (CpuBackend + virtio-gpu 2D)
      virgil-render/         — GPU render service (Virgil3D/Gallium3D via QEMU)
      virtio-input/main.rs   — keyboard + tablet input (evdev translation, IPC forwarding)
      virtio-9p/main.rs      — host filesystem passthrough (9P2000.L protocol)
      virtio-blk/main.rs     — block device driver (reads sectors)
      virtio-console/main.rs — console driver (placeholder)

  libraries/                 — shared libraries (compiled as rlibs)
    sys/                     — syscall wrappers + GlobalAlloc + panic handler
    virtio/                  — virtio MMIO transport + split virtqueue
    drawing/                 — surfaces, colors, PNG decoder, compositing, palette
    fonts/                   — TrueType rasterizer, stem darkening, glyph cache
    animation/               — easing functions, spring physics, timeline sequencing
    layout/                  — unified text layout engine (mono + proportional)
    render/                  — render backend (CpuBackend, scene walk, damage, frame scheduler)
    scene/                   — scene graph nodes, triple-buffered shared memory layout
    ipc/                     — lock-free SPSC ring buffers on shared memory
    protocol/                — IPC message types + payload structs (all protocol boundaries)
    link.ld                  — shared userspace linker script (base VA 0x400000)

  user/                      — user programs
    text-editor/main.rs      — editor process (input → write requests via IPC)
    echo/main.rs             — echo process (IPC demo)
    stress/main.rs           — IPC stress test program
    fuzz/main.rs             — fuzzing harness
    fuzz-helper/main.rs      — fuzzing helper
```
